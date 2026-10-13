//! 8-bit weight-only quantization of the embedding model, done by the service after the
//! download (design, "As built in M10" → "An 8-bit model").
//!
//! Every `MatMul` whose second input is a constant 2-D float matrix becomes onnxruntime's
//! `MatMulNBits` (domain `com.microsoft`): the weights in 8 bits per value, in blocks of 32
//! along K, one scale per block, symmetric, computed at accuracy level 4 (int8). Rounding
//! follows onnxruntime's own quantizer (`MatMulNBitsQuantizer`, `DefaultWeightOnlyQuantConfig`
//! with `bits=8, block_size=32, is_symmetric=True, accuracy_level=4`), so the weights come out
//! as that tool makes them, value for value. The rest of the file is copied byte for byte: the
//! model is edited at the protobuf wire level, without a schema, so nothing it holds is lost.
//! The result is deterministic; the caller checks it against a known hash.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, bail};

/// Values per block of K sharing a scale.
const BLOCK: usize = 32;
const BITS: i64 = 8;
/// int8 computation (`MatMulNBits`' `accuracy_level`): the fast kernel on CPUs.
const ACCURACY_LEVEL: i64 = 4;
/// The quantized value standing for 0.
const ZERO_POINT: f32 = 128.0;

/// `model` (an ONNX `ModelProto`) with its constant `MatMul` weights in 8 bits.
pub fn quantize(model: &[u8]) -> anyhow::Result<Vec<u8>> {
    let top = fields(model).context("reading the model")?;
    let mut out = Vec::with_capacity(model.len() / 2);
    let mut has_ms_opset = false;
    let mut graphs = 0;
    for f in &top {
        match (f.num, &f.value) {
            // ModelProto.graph
            (7, Value::Bytes(graph)) => {
                put_bytes(&mut out, 7, &quantize_graph(graph)?);
                graphs += 1;
            }
            // ModelProto.opset_import
            (8, Value::Bytes(opset)) => {
                has_ms_opset |= fields(opset)?
                    .iter()
                    .any(|o| o.num == 1 && o.bytes() == Some(b"com.microsoft"));
                out.extend_from_slice(f.raw);
            }
            _ => out.extend_from_slice(f.raw),
        }
    }
    if graphs != 1 {
        bail!("the model has {graphs} graphs, not one");
    }
    if !has_ms_opset {
        let mut opset = Vec::new();
        put_bytes(&mut opset, 1, b"com.microsoft");
        put_varint_field(&mut opset, 2, 1);
        put_bytes(&mut out, 8, &opset);
    }
    Ok(out)
}

/// A `GraphProto` with its `MatMul`s on constant 2-D float weights replaced.
fn quantize_graph(graph: &[u8]) -> anyhow::Result<Vec<u8>> {
    let gf = fields(graph).context("reading the graph")?;
    let mut inits: HashMap<&str, Tensor> = HashMap::new();
    let mut nodes = Vec::new();
    for f in &gf {
        match (f.num, &f.value) {
            (1, Value::Bytes(b)) => nodes.push(Node::parse(b)?),
            (5, Value::Bytes(b)) => {
                let t = Tensor::parse(b)?;
                inits.insert(t.name, t);
            }
            _ => {}
        }
    }
    let mut uses: HashMap<&str, usize> = HashMap::new();
    for n in &nodes {
        for i in &n.inputs {
            *uses.entry(i).or_default() += 1;
        }
    }
    // Replacement node bytes by node index, the weights they replace, and the new weights.
    let mut replaced: HashMap<usize, Vec<u8>> = HashMap::new();
    let mut removed: HashSet<&str> = HashSet::new();
    let mut added = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        if n.op_type != "MatMul" || !n.domain.is_empty() || n.inputs.len() != 2 {
            continue;
        }
        let Some(b) = inits.get(n.inputs[1]) else {
            continue;
        };
        let Some((k, cols, values)) = b.float_matrix()? else {
            continue;
        };
        let (packed, scales) = quantize_matrix(&values, k, cols);
        let blocks = k.div_ceil(BLOCK);
        let q_name = format!("{}_Q8", b.name);
        let s_name = format!("{}_scales", b.name);
        added.push(tensor(
            &q_name,
            &[cols, blocks, BLOCK],
            TENSOR_UINT8,
            &packed,
        ));
        let scale_bytes: Vec<u8> = scales.iter().flat_map(|s| s.to_le_bytes()).collect();
        added.push(tensor(&s_name, &[cols, blocks], TENSOR_FLOAT, &scale_bytes));
        replaced.insert(i, n.nbits(&q_name, &s_name, k, cols));
        if uses[b.name] == 1 {
            removed.insert(b.name);
        }
    }
    let mut out = Vec::with_capacity(graph.len() / 2);
    let mut node_index = 0;
    for f in &gf {
        match (f.num, &f.value) {
            (1, _) => {
                match replaced.get(&node_index) {
                    Some(node) => put_bytes(&mut out, 1, node),
                    None => out.extend_from_slice(f.raw),
                }
                node_index += 1;
            }
            (5, Value::Bytes(b)) if removed.contains(Tensor::parse(b)?.name) => {}
            // GraphProto.input: an older model lists its weights among the inputs too.
            (11, Value::Bytes(b)) if removed.contains(value_info_name(b)?) => {}
            _ => out.extend_from_slice(f.raw),
        }
    }
    for t in &added {
        put_bytes(&mut out, 5, t);
    }
    Ok(out)
}

/// Block-wise symmetric 8-bit quantization of a row-major `k` × `cols` matrix, as
/// onnxruntime's MLAS `BlockwiseQuantizer` does it: per column, blocks of 32 along K; the
/// scale is the block's value of largest magnitude over -128 (so that value becomes -128, the
/// wider side), and each value is `round(v / scale + 128)` clamped to 0..=255, multiplying by
/// the reciprocal of the scale as MLAS does. Returns the bytes `[cols][blocks][32]` and the
/// scales `[cols][blocks]`.
fn quantize_matrix(w: &[f32], k: usize, cols: usize) -> (Vec<u8>, Vec<f32>) {
    let blocks = k.div_ceil(BLOCK);
    let columns: Vec<usize> = (0..cols).collect();
    let per_column = crate::par::map(columns, |c| {
        let mut packed = vec![ZERO_POINT as u8; blocks * BLOCK];
        let mut scales = vec![0f32; blocks];
        for b in 0..blocks {
            let rows = b * BLOCK..(k.min((b + 1) * BLOCK));
            let (mut min, mut max) = (f32::MAX, f32::MIN);
            for r in rows.clone() {
                let v = w[r * cols + c];
                min = min.min(v);
                max = max.max(v);
            }
            let wide = if max.abs() > min.abs() { max } else { min };
            let scale = -wide / ZERO_POINT;
            let reciprocal = if scale != 0.0 { 1.0 / scale } else { 0.0 };
            for r in rows {
                let q = (w[r * cols + c] * reciprocal + ZERO_POINT)
                    .round()
                    .clamp(0.0, 255.0);
                packed[b * BLOCK + r % BLOCK] = q as u8;
            }
            scales[b] = scale;
        }
        (packed, scales)
    });
    let mut packed = Vec::with_capacity(cols * blocks * BLOCK);
    let mut scales = Vec::with_capacity(cols * blocks);
    for (p, s) in per_column {
        packed.extend(p);
        scales.extend(s);
    }
    (packed, scales)
}

// TensorProto.DataType
const TENSOR_FLOAT: u64 = 1;
const TENSOR_UINT8: u64 = 2;

/// The parts of a `TensorProto` the quantizer reads.
struct Tensor<'a> {
    name: &'a str,
    dims: Vec<u64>,
    data_type: u64,
    raw: Option<&'a [u8]>,
    /// `float_data`, packed or not.
    floats: Vec<f32>,
    external: bool,
}

impl<'a> Tensor<'a> {
    fn parse(buf: &'a [u8]) -> anyhow::Result<Self> {
        let mut t = Tensor {
            name: "",
            dims: Vec::new(),
            data_type: 0,
            raw: None,
            floats: Vec::new(),
            external: false,
        };
        for f in fields(buf)? {
            match (f.num, f.value) {
                (1, Value::Varint(d)) => t.dims.push(d),
                (1, Value::Bytes(b)) => t.dims.extend(packed_varints(b)?),
                (2, Value::Varint(d)) => t.data_type = d,
                (4, Value::Fixed32(v)) => t.floats.push(f32::from_bits(v)),
                (4, Value::Bytes(b)) => t.floats.extend(floats(b)),
                (8, Value::Bytes(b)) => t.name = utf8(b)?,
                (9, Value::Bytes(b)) => t.raw = Some(b),
                (13, _) => t.external = true,
                (14, Value::Varint(1)) => t.external = true,
                _ => {}
            }
        }
        Ok(t)
    }

    /// `(k, cols, values)` when this is a 2-D float matrix held in the file; `None` otherwise.
    fn float_matrix(&self) -> anyhow::Result<Option<(usize, usize, Vec<f32>)>> {
        if self.data_type != TENSOR_FLOAT || self.dims.len() != 2 || self.external {
            return Ok(None);
        }
        let (k, cols) = (self.dims[0] as usize, self.dims[1] as usize);
        let values: Vec<f32> = match self.raw {
            Some(raw) => floats(raw).collect(),
            None => self.floats.clone(),
        };
        if values.len() != k * cols || k == 0 || cols == 0 {
            bail!(
                "weight {} holds {} values for {k} x {cols}",
                self.name,
                values.len()
            );
        }
        Ok(Some((k, cols, values)))
    }
}

/// The parts of a `NodeProto` the quantizer reads.
struct Node<'a> {
    inputs: Vec<&'a str>,
    outputs: Vec<&'a str>,
    name: &'a str,
    op_type: &'a str,
    domain: &'a str,
}

impl<'a> Node<'a> {
    fn parse(buf: &'a [u8]) -> anyhow::Result<Self> {
        let mut n = Node {
            inputs: Vec::new(),
            outputs: Vec::new(),
            name: "",
            op_type: "",
            domain: "",
        };
        for f in fields(buf)? {
            if let Value::Bytes(b) = f.value {
                match f.num {
                    1 => n.inputs.push(utf8(b)?),
                    2 => n.outputs.push(utf8(b)?),
                    3 => n.name = utf8(b)?,
                    4 => n.op_type = utf8(b)?,
                    7 => n.domain = utf8(b)?,
                    _ => {}
                }
            }
        }
        Ok(n)
    }

    /// This `MatMul` as a `MatMulNBits` node on the quantized weight and its scales, with the
    /// same outputs; attributes in name order, as onnx's `make_node` writes them.
    fn nbits(&self, weight: &str, scales: &str, k: usize, cols: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for input in [self.inputs[0], weight, scales] {
            put_bytes(&mut out, 1, input.as_bytes());
        }
        for output in &self.outputs {
            put_bytes(&mut out, 2, output.as_bytes());
        }
        if !self.name.is_empty() {
            put_bytes(&mut out, 3, format!("{}_Q8", self.name).as_bytes());
        }
        put_bytes(&mut out, 4, b"MatMulNBits");
        for (name, value) in [
            ("K", k as i64),
            ("N", cols as i64),
            ("accuracy_level", ACCURACY_LEVEL),
            ("bits", BITS),
            ("block_size", BLOCK as i64),
        ] {
            // AttributeProto: name, i, type INT (2)
            let mut attr = Vec::new();
            put_bytes(&mut attr, 1, name.as_bytes());
            put_varint_field(&mut attr, 3, value as u64);
            put_varint_field(&mut attr, 20, 2);
            put_bytes(&mut out, 5, &attr);
        }
        put_bytes(&mut out, 7, b"com.microsoft");
        out
    }
}

/// A `TensorProto` with its values in `raw_data`.
fn tensor(name: &str, dims: &[usize], data_type: u64, raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + name.len() + 32);
    for &d in dims {
        put_varint_field(&mut out, 1, d as u64);
    }
    put_varint_field(&mut out, 2, data_type);
    put_bytes(&mut out, 8, name.as_bytes());
    put_bytes(&mut out, 9, raw);
    out
}

/// A `ValueInfoProto`'s name.
fn value_info_name(buf: &[u8]) -> anyhow::Result<&str> {
    for f in fields(buf)? {
        if let (1, Value::Bytes(b)) = (f.num, f.value) {
            return utf8(b);
        }
    }
    Ok("")
}

/// Little-endian floats.
fn floats(b: &[u8]) -> impl Iterator<Item = f32> + '_ {
    b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c))
}

fn utf8(b: &[u8]) -> anyhow::Result<&str> {
    std::str::from_utf8(b).context("a name that isn't UTF-8")
}

// The protobuf wire format: just enough to walk messages and write the few new ones.

#[derive(Clone, Copy)]
enum Value<'a> {
    Varint(u64),
    /// Skipped: no field the quantizer reads has this type.
    Fixed64,
    Bytes(&'a [u8]),
    Fixed32(u32),
}

/// One field of a message: its number, value and its bytes as they stand in the message (tag
/// included), for copying it unchanged.
struct Field<'a> {
    num: u64,
    value: Value<'a>,
    raw: &'a [u8],
}

impl<'a> Field<'a> {
    fn bytes(&self) -> Option<&'a [u8]> {
        match self.value {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

fn varint(buf: &[u8], pos: &mut usize) -> anyhow::Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let Some(&byte) = buf.get(*pos) else {
            bail!("the file ends inside a number");
        };
        *pos += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    bail!("a number longer than 64 bits")
}

fn take<'a>(buf: &'a [u8], pos: &mut usize, n: usize) -> anyhow::Result<&'a [u8]> {
    let end = pos
        .checked_add(n)
        .filter(|&e| e <= buf.len())
        .context("a field runs past the end of its message")?;
    let out = &buf[*pos..end];
    *pos = end;
    Ok(out)
}

/// The fields of one message, in order.
fn fields(buf: &[u8]) -> anyhow::Result<Vec<Field<'_>>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let start = pos;
        let tag = varint(buf, &mut pos)?;
        let value = match tag & 7 {
            0 => Value::Varint(varint(buf, &mut pos)?),
            1 => {
                take(buf, &mut pos, 8)?;
                Value::Fixed64
            }
            2 => {
                let n = usize::try_from(varint(buf, &mut pos)?)?;
                Value::Bytes(take(buf, &mut pos, n)?)
            }
            5 => {
                let b = take(buf, &mut pos, 4)?;
                Value::Fixed32(u32::from_le_bytes(b.try_into().expect("4 bytes")))
            }
            w => bail!("unsupported wire type {w}"),
        };
        out.push(Field {
            num: tag >> 3,
            value,
            raw: &buf[start..pos],
        });
    }
    Ok(out)
}

fn packed_varints(buf: &[u8]) -> anyhow::Result<Vec<u64>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        out.push(varint(buf, &mut pos)?);
    }
    Ok(out)
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_varint_field(out: &mut Vec<u8>, num: u64, v: u64) {
    put_varint(out, num << 3);
    put_varint(out, v);
}

fn put_bytes(out: &mut Vec<u8>, num: u64, b: &[u8]) {
    put_varint(out, (num << 3) | 2);
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(inputs: &[&str], output: &str, name: &str, op: &str) -> Vec<u8> {
        let mut n = Vec::new();
        for i in inputs {
            put_bytes(&mut n, 1, i.as_bytes());
        }
        put_bytes(&mut n, 2, output.as_bytes());
        put_bytes(&mut n, 3, name.as_bytes());
        put_bytes(&mut n, 4, op.as_bytes());
        n
    }

    /// A model of one `MatMul` on a constant 40 x 3 weight (two blocks, the second short), one
    /// `MatMul` of two inputs, and a field the quantizer knows nothing about.
    fn model(w: &[f32]) -> Vec<u8> {
        let mut g = Vec::new();
        put_bytes(&mut g, 1, &node(&["x", "w"], "y", "mm", "MatMul"));
        put_bytes(&mut g, 1, &node(&["y", "z"], "out", "mm2", "MatMul"));
        put_bytes(&mut g, 2, b"g");
        let raw: Vec<u8> = w.iter().flat_map(|v| v.to_le_bytes()).collect();
        put_bytes(&mut g, 5, &tensor("w", &[40, 3], TENSOR_FLOAT, &raw));
        let mut vi = Vec::new();
        put_bytes(&mut vi, 1, b"w");
        put_bytes(&mut g, 11, &vi);
        let mut m = Vec::new();
        put_varint_field(&mut m, 1, 6);
        put_bytes(&mut m, 7, &g);
        let mut opset = Vec::new();
        put_varint_field(&mut opset, 2, 11);
        put_bytes(&mut m, 8, &opset);
        put_bytes(&mut m, 99, b"kept as it is");
        m
    }

    #[test]
    fn a_constant_matmul_becomes_matmulnbits_and_the_rest_stays() {
        let w: Vec<f32> = (0..120)
            .map(|i| ((i * 37 % 101) as f32 - 50.0) / 7.0)
            .collect();
        let out = quantize(&model(&w)).unwrap();
        let top = fields(&out).unwrap();
        assert!(
            top.iter()
                .any(|f| f.num == 99 && f.bytes() == Some(b"kept as it is"))
        );
        let opsets: Vec<_> = top.iter().filter(|f| f.num == 8).collect();
        assert_eq!(opsets.len(), 2);
        let graph = top.iter().find(|f| f.num == 7).unwrap().bytes().unwrap();
        let gf = fields(graph).unwrap();
        let nodes: Vec<Node> = gf
            .iter()
            .filter(|f| f.num == 1)
            .map(|f| Node::parse(f.bytes().unwrap()).unwrap())
            .collect();
        assert_eq!(nodes[0].op_type, "MatMulNBits");
        assert_eq!(nodes[0].domain, "com.microsoft");
        assert_eq!(nodes[0].inputs, ["x", "w_Q8", "w_scales"]);
        assert_eq!((nodes[0].outputs[0], nodes[0].name), ("y", "mm_Q8"));
        assert_eq!(nodes[1].op_type, "MatMul");
        let tensors: HashMap<&str, Tensor> = gf
            .iter()
            .filter(|f| f.num == 5)
            .map(|f| {
                let t = Tensor::parse(f.bytes().unwrap()).unwrap();
                (t.name, t)
            })
            .collect();
        assert!(!tensors.contains_key("w"), "the float weight is gone");
        assert!(!gf.iter().any(|f| f.num == 11), "and so is its input entry");
        let q = &tensors["w_Q8"];
        let s = &tensors["w_scales"];
        assert_eq!(
            (q.dims.as_slice(), q.data_type),
            (&[3, 2, 32][..], TENSOR_UINT8)
        );
        assert_eq!(s.dims, [3, 2]);
        // Every value comes back within half a step, except on the narrow side: 0..=255 holds
        // 128 steps below the zero point and 127 above, so a value there as wide as the
        // block's widest is clamped, a step off (as onnxruntime's quantizer does it).
        let scales: Vec<f32> = floats(s.raw.unwrap()).collect();
        let q = q.raw.unwrap();
        for c in 0..3 {
            for r in 0..40 {
                let (b, i) = (r / 32, r % 32);
                let scale = scales[c * 2 + b];
                let stored = q[(c * 2 + b) * 32 + i];
                let back = (f32::from(stored) - 128.0) * scale;
                let off = if stored == 255 { 1.0 } else { 0.5 };
                assert!(
                    (back - w[r * 3 + c]).abs() <= scale.abs() * off + 1e-6,
                    "{r},{c}"
                );
            }
        }
        // Padding past K stands for 0.
        assert_eq!(q[32 + 8..64], [128; 24]);
        // Deterministic.
        assert_eq!(out, quantize(&model(&w)).unwrap());
    }

    #[test]
    fn a_block_of_zeros_quantizes_to_the_zero_point() {
        let (q, s) = quantize_matrix(&[0.0; 32], 32, 1);
        assert_eq!((q, s), (vec![128; 32], vec![0.0]));
    }
}
