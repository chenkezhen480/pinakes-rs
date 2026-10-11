//! Word (`.docx`) to Markdown: headings from paragraph styles (or their outline level), lists
//! from numbering, tables, links, bold and italic, footnotes and endnotes as notes at the end,
//! text boxes after their paragraph. Deleted (tracked) and hidden text is left out; images and
//! field codes are dropped.

use std::collections::HashMap;

use super::md::{self, Md};
use super::package::{Package, relationships};
use super::xml::{Element, Node};
use super::{Limits, Refused};

#[derive(Default, Clone)]
struct Style {
    name: String,
    based_on: Option<String>,
    outline: Option<usize>,
    num: Option<(String, usize)>,
}

struct Doc {
    styles: HashMap<String, Style>,
    /// numId → the format of each level (`bullet`, `decimal`, ...).
    numbering: HashMap<String, Vec<String>>,
    links: HashMap<String, (String, bool)>,
    notes: HashMap<String, String>,
    /// Notes referenced so far, in order: (label, text).
    cited: Vec<(String, String)>,
    /// The labels in `cited`, for checking without a scan.
    cited_labels: std::collections::HashSet<String>,
    /// Counters of numbered lists, by numId, per level.
    counters: HashMap<String, Vec<usize>>,
    /// Text boxes met inside the paragraph being written, written after it.
    boxes: Vec<Element>,
}

pub fn convert(bytes: &[u8], limits: &Limits) -> Result<String, Refused> {
    let mut pkg = Package::open(bytes, limits)?;
    let document = pkg
        .xml("word/document.xml")?
        .ok_or_else(|| Refused::Unreadable("not a Word document: no word/document.xml".into()))?;
    let mut doc = Doc {
        styles: styles(pkg.xml("word/styles.xml")?),
        numbering: numbering(pkg.xml("word/numbering.xml")?),
        links: relationships(pkg.xml("word/_rels/document.xml.rels")?),
        notes: HashMap::new(),
        cited: Vec::new(),
        cited_labels: std::collections::HashSet::new(),
        counters: HashMap::new(),
        boxes: Vec::new(),
    };
    for (part, kind, prefix) in [
        ("word/footnotes.xml", "footnote", ""),
        ("word/endnotes.xml", "endnote", "e"),
    ] {
        if let Some(root) = pkg.xml(part)? {
            let mut found = Vec::new();
            root.find_all(kind, &mut found);
            for n in found {
                let (Some(id), None) = (n.attr("id"), n.attr("type")) else {
                    continue;
                };
                let text: Vec<String> = n
                    .elements()
                    .filter(|p| p.name == "p")
                    .map(|p| md::collapse(&doc.inline(p)))
                    .filter(|t| !t.is_empty())
                    .collect();
                doc.notes.insert(format!("{prefix}{id}"), text.join(" "));
            }
        }
    }
    let body = document
        .path(&["document", "body"])
        .ok_or_else(|| Refused::Unreadable("not a Word document: no body".into()))?;
    let mut out = Md::new();
    doc.blocks(body, &mut out, limits)?;
    for (label, text) in std::mem::take(&mut doc.cited) {
        out.paragraph(&format!("[^{label}]: {text}"));
    }
    Ok(out.finish())
}

fn styles(root: Option<Element>) -> HashMap<String, Style> {
    let mut out = HashMap::new();
    let Some(root) = root else {
        return out;
    };
    let mut found = Vec::new();
    root.find_all("style", &mut found);
    for s in found {
        let Some(id) = s.attr("styleId") else {
            continue;
        };
        let ppr = s.child("pPr");
        out.insert(
            id.to_owned(),
            Style {
                name: s
                    .child("name")
                    .and_then(|n| n.attr("val"))
                    .unwrap_or_default()
                    .to_owned(),
                based_on: s
                    .child("basedOn")
                    .and_then(|b| b.attr("val"))
                    .map(str::to_owned),
                outline: ppr
                    .and_then(|p| p.child("outlineLvl"))
                    .and_then(|o| o.attr("val"))
                    .and_then(|v| v.parse().ok()),
                num: ppr.and_then(num_pr),
            },
        );
    }
    out
}

fn numbering(root: Option<Element>) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    let Some(root) = root else {
        return out;
    };
    let Some(numbering) = root.child("numbering") else {
        return out;
    };
    let mut abstracts: HashMap<&str, Vec<String>> = HashMap::new();
    for a in numbering.elements().filter(|e| e.name == "abstractNum") {
        let Some(id) = a.attr("abstractNumId") else {
            continue;
        };
        let mut levels = vec![String::from("decimal"); 9];
        for lvl in a.elements().filter(|e| e.name == "lvl") {
            let i: usize = lvl.attr("ilvl").and_then(|v| v.parse().ok()).unwrap_or(0);
            if let (Some(slot), Some(fmt)) = (
                levels.get_mut(i),
                lvl.child("numFmt").and_then(|f| f.attr("val")),
            ) {
                *slot = fmt.to_owned();
            }
        }
        abstracts.insert(id, levels);
    }
    for n in numbering.elements().filter(|e| e.name == "num") {
        if let (Some(id), Some(levels)) = (
            n.attr("numId"),
            n.child("abstractNumId")
                .and_then(|a| a.attr("val"))
                .and_then(|a| abstracts.get(a)),
        ) {
            out.insert(id.to_owned(), levels.clone());
        }
    }
    out
}

fn num_pr(ppr: &Element) -> Option<(String, usize)> {
    let num = ppr.child("numPr")?;
    let id = num.child("numId")?.attr("val")?;
    let level = num
        .child("ilvl")
        .and_then(|l| l.attr("val"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    Some((id.to_owned(), level))
}

/// Whether a run property is set (`<w:b/>`, `<w:b w:val="true"/>`, not `w:val="0"`).
fn on(rpr: Option<&Element>, name: &str) -> bool {
    rpr.and_then(|r| r.child(name))
        .is_some_and(|e| !matches!(e.attr("val"), Some("0" | "false" | "none")))
}

/// Runs of text with the same formatting.
struct Inline {
    parts: Vec<(String, bool, bool)>,
}

impl Inline {
    fn push(&mut self, text: &str, bold: bool, italic: bool) {
        if text.is_empty() {
            return;
        }
        match self.parts.last_mut() {
            Some((t, b, i)) if *b == bold && *i == italic => t.push_str(text),
            _ => self.parts.push((text.to_owned(), bold, italic)),
        }
    }

    fn markdown(self) -> String {
        let mut out = String::new();
        for (text, bold, italic) in self.parts {
            let mark = match (bold, italic) {
                (true, true) => "***",
                (true, false) => "**",
                (false, true) => "*",
                (false, false) => "",
            };
            if mark.is_empty() || text.trim().is_empty() {
                out.push_str(&text);
                continue;
            }
            // Spaces stay outside the marks, or they don't count as emphasis.
            let core = text.trim();
            let lead = &text[..text.len() - text.trim_start().len()];
            let trail = &text[text.trim_end().len()..];
            out.push_str(lead);
            out.push_str(mark);
            out.push_str(core);
            out.push_str(mark);
            out.push_str(trail);
        }
        out
    }
}

impl Doc {
    fn style_chain(&self, id: Option<&str>) -> Vec<&Style> {
        let mut chain = Vec::new();
        let mut next = id.map(str::to_owned);
        while let Some(id) = next {
            let Some(s) = self.styles.get(&id) else {
                break;
            };
            if chain.len() > 16 {
                break;
            }
            chain.push(s);
            next = s.based_on.clone();
        }
        chain
    }

    /// The heading level of a paragraph, if it is one.
    fn heading(&self, p: &Element) -> Option<usize> {
        let ppr = p.child("pPr");
        if let Some(lvl) = ppr
            .and_then(|pp| pp.child("outlineLvl"))
            .and_then(|o| o.attr("val"))
            .and_then(|v| v.parse::<usize>().ok())
        {
            return (lvl < 9).then_some(lvl + 1);
        }
        let id = ppr
            .and_then(|pp| pp.child("pStyle"))
            .and_then(|s| s.attr("val"));
        for s in self.style_chain(id) {
            let name = s.name.to_lowercase();
            if name == "title" {
                return Some(1);
            }
            if let Some(n) = name
                .strip_prefix("heading ")
                .and_then(|n| n.trim().parse::<usize>().ok())
            {
                return Some(n.clamp(1, 6));
            }
            if let Some(lvl) = s.outline {
                return (lvl < 9).then_some(lvl + 1);
            }
        }
        None
    }

    /// The list a paragraph belongs to: (numId, level).
    fn list(&self, p: &Element) -> Option<(String, usize)> {
        let ppr = p.child("pPr");
        let own = ppr.and_then(num_pr);
        let id = ppr
            .and_then(|pp| pp.child("pStyle"))
            .and_then(|s| s.attr("val"));
        let found = own.or_else(|| self.style_chain(id).iter().find_map(|s| s.num.clone()));
        found.filter(|(id, _)| id != "0")
    }

    fn blocks(&mut self, parent: &Element, out: &mut Md, limits: &Limits) -> Result<(), Refused> {
        for el in parent.elements() {
            if out.len() > limits.markdown_bytes {
                return Err(Refused::TooLarge(format!(
                    "the text is longer than {} MB",
                    limits.markdown_bytes >> 20
                )));
            }
            match el.name.as_str() {
                "p" => self.paragraph(el, out, limits)?,
                "tbl" => {
                    let rows = self.table(el);
                    out.table(&rows);
                }
                "sdt" => {
                    if let Some(content) = el.child("sdtContent") {
                        self.blocks(content, out, limits)?;
                    }
                }
                "customXml" | "ins" | "moveTo" => self.blocks(el, out, limits)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn paragraph(&mut self, p: &Element, out: &mut Md, limits: &Limits) -> Result<(), Refused> {
        let text = self.inline(p);
        let boxes = std::mem::take(&mut self.boxes);
        if let Some(level) = self.heading(p) {
            // Emphasis marks inside a heading add nothing.
            out.heading(level, &text.replace("**", "").replace('*', ""));
        } else if let Some((id, level)) = self.list(p) {
            let format = self
                .numbering
                .get(&id)
                .and_then(|l| l.get(level))
                .map(String::as_str)
                .unwrap_or("bullet");
            let number = (format != "bullet" && format != "none").then(|| {
                let c = self
                    .counters
                    .entry(id.clone())
                    .or_insert_with(|| vec![0; 9]);
                let level = level.min(8);
                c[level] += 1;
                for deeper in &mut c[level + 1..] {
                    *deeper = 0;
                }
                c[level]
            });
            out.item(level, number, &md::collapse(&text));
        } else {
            let lines: Vec<String> = text
                .split('\n')
                .map(md::collapse)
                .filter(|l| !l.is_empty())
                .collect();
            if !lines.is_empty() {
                out.paragraph(&md::broken_lines(&lines));
            }
        }
        for b in boxes {
            self.blocks(&b, out, limits)?;
        }
        Ok(())
    }

    /// A paragraph's text as inline Markdown; line breaks as `\n`.
    fn inline(&mut self, p: &Element) -> String {
        let mut inline = Inline { parts: Vec::new() };
        self.runs(p, &mut inline, false, false);
        inline.markdown()
    }

    fn runs(&mut self, parent: &Element, out: &mut Inline, bold: bool, italic: bool) {
        for el in parent.elements() {
            match el.name.as_str() {
                "r" => self.run(el, out, bold, italic),
                "hyperlink" => {
                    let target = el
                        .attr("id")
                        .and_then(|id| self.links.get(id))
                        .filter(|(_, external)| *external)
                        .map(|(t, _)| t.clone())
                        .filter(|t| !t.trim_start().to_lowercase().starts_with("javascript:"));
                    let mut inner = Inline { parts: Vec::new() };
                    self.runs(el, &mut inner, false, false);
                    let text = md::collapse(&inner.markdown());
                    match target {
                        Some(t) if !text.is_empty() => out.push(
                            &format!("[{text}]({})", t.replace(' ', "%20")),
                            bold,
                            italic,
                        ),
                        _ => out.push(&text, bold, italic),
                    }
                }
                "ins" | "moveTo" | "smartTag" | "customXml" | "fldSimple" | "bdo" | "dir" => {
                    self.runs(el, out, bold, italic)
                }
                "sdt" => {
                    if let Some(c) = el.child("sdtContent") {
                        self.runs(c, out, bold, italic);
                    }
                }
                "oMath" | "oMathPara" => out.push(&el.text(), bold, italic),
                _ => {}
            }
        }
    }

    fn run(&mut self, r: &Element, out: &mut Inline, bold: bool, italic: bool) {
        let rpr = r.child("rPr");
        if on(rpr, "vanish") {
            return;
        }
        let bold = bold || on(rpr, "b");
        let italic = italic || on(rpr, "i");
        for el in r.elements() {
            match el.name.as_str() {
                "t" => out.push(&el.text(), bold, italic),
                "tab" | "ptab" => out.push(" ", bold, italic),
                "br" | "cr" => {
                    if el.attr("type") != Some("page") {
                        out.push("\n", false, false);
                    }
                }
                "noBreakHyphen" => out.push("-", bold, italic),
                "sym" => out.push(" ", bold, italic),
                "footnoteReference" | "endnoteReference" => {
                    let prefix = if el.name == "endnoteReference" {
                        "e"
                    } else {
                        ""
                    };
                    if let Some(id) = el.attr("id") {
                        let label = format!("{prefix}{id}");
                        if let Some(text) = self.notes.get(&label).cloned() {
                            out.push(&format!("[^{label}]"), false, false);
                            if self.cited_labels.insert(label.clone()) {
                                self.cited.push((label, text));
                            }
                        }
                    }
                }
                "drawing" | "pict" | "AlternateContent" => {
                    let mut boxes = Vec::new();
                    collect_boxes(el, &mut boxes);
                    self.boxes.extend(boxes);
                }
                _ => {}
            }
        }
    }

    fn table(&mut self, tbl: &Element) -> Vec<Vec<String>> {
        let mut rows = Vec::new();
        for tr in tbl.elements().filter(|e| e.name == "tr") {
            let mut row = Vec::new();
            for tc in tr.elements().filter(|e| e.name == "tc" || e.name == "sdt") {
                let tc = if tc.name == "sdt" {
                    match tc.path(&["sdtContent", "tc"]) {
                        Some(c) => c,
                        None => continue,
                    }
                } else {
                    tc
                };
                let mut parts = Vec::new();
                for el in tc.elements() {
                    match el.name.as_str() {
                        "p" => parts.push(md::collapse(&self.inline(el))),
                        // A table in a cell is flattened to its text.
                        "tbl" => parts.push(md::collapse(&el.text())),
                        _ => {}
                    }
                }
                self.boxes.clear();
                row.push(
                    parts
                        .into_iter()
                        .filter(|p| !p.is_empty())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
                let span: usize = tc
                    .path(&["tcPr", "gridSpan"])
                    .and_then(|g| g.attr("val"))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1);
                for _ in 1..span.min(64) {
                    row.push(String::new());
                }
            }
            rows.push(row);
        }
        rows
    }
}

/// The text boxes in a drawing: their content, once (the first of an alternate content's
/// choices only, as the others repeat it).
fn collect_boxes(el: &Element, out: &mut Vec<Element>) {
    for child in el.elements() {
        match child.name.as_str() {
            "txbxContent" => out.push(Element {
                name: "txbxContent".into(),
                attrs: Vec::new(),
                children: child
                    .children
                    .iter()
                    .filter_map(|n| match n {
                        Node::Element(e) if e.name == "p" || e.name == "tbl" => {
                            Some(Node::Element(clone(e)))
                        }
                        _ => None,
                    })
                    .collect(),
            }),
            "Fallback" => {}
            "AlternateContent" => {
                if let Some(choice) = child.child("Choice") {
                    collect_boxes(choice, out);
                }
            }
            _ => collect_boxes(child, out),
        }
    }
}

fn clone(e: &Element) -> Element {
    Element {
        name: e.name.clone(),
        attrs: e.attrs.clone(),
        children: e
            .children
            .iter()
            .map(|n| match n {
                Node::Element(e) => Node::Element(clone(e)),
                Node::Text(t) => Node::Text(t.clone()),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::package::tests::zip_of;
    use super::*;

    const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    fn docx(body: &str, extra: &[(&str, &str)]) -> Vec<u8> {
        let document =
            format!(r#"<?xml version="1.0"?><w:document {W}><w:body>{body}</w:body></w:document>"#);
        let mut entries: Vec<(&str, &[u8])> = vec![("word/document.xml", document.as_bytes())];
        for (n, t) in extra {
            entries.push((n, t.as_bytes()));
        }
        zip_of(&entries)
    }

    #[test]
    fn headings_lists_tables_links_and_notes() {
        let styles = format!(
            r#"<w:styles {W}><w:style w:styleId="1"><w:name w:val="heading 1"/></w:style>
<w:style w:styleId="Sub"><w:name w:val="My Sub"/><w:basedOn w:val="H2"/></w:style>
<w:style w:styleId="H2"><w:name w:val="heading 2"/></w:style></w:styles>"#
        );
        let numbering = format!(
            r#"<w:numbering {W}><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl><w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum><w:num w:numId="5"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
        );
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="hyperlink" Target="https://example.com/a b" TargetMode="External"/></Relationships>"#;
        let notes = format!(
            r#"<w:footnotes {W}><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:t>---</w:t></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:t>Seven calendar days.</w:t></w:r></w:p></w:footnote></w:footnotes>"#
        );
        let item = |lvl: u8, text: &str| {
            format!(
                r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{lvl}"/><w:numId w:val="5"/></w:numPr></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
            )
        };
        let body = format!(
            r#"<w:p><w:pPr><w:pStyle w:val="1"/></w:pPr><w:r><w:t>退货政策</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">Goods may be returned within </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>seven days</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r><w:r><w:t>. See </w:t></w:r><w:hyperlink r:id="rId9"><w:r><w:t>the terms</w:t></w:r></w:hyperlink><w:del><w:r><w:delText>deleted</w:delText></w:r></w:del><w:r><w:rPr><w:vanish/></w:rPr><w:t>hidden</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Sub"/></w:pPr><w:r><w:t>Steps</w:t></w:r></w:p>
{}{}{}
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Item</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Days</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>All | any</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:p><w:r><w:t># not a heading</w:t></w:r></w:p>"#,
            item(0, "Ask"),
            item(1, "by mail"),
            item(0, "Send"),
        );
        let md = convert(
            &docx(
                &body,
                &[
                    ("word/styles.xml", &styles),
                    ("word/numbering.xml", &numbering),
                    ("word/_rels/document.xml.rels", rels),
                    ("word/footnotes.xml", &notes),
                ],
            ),
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(
            md,
            "# 退货政策\n\nGoods may be returned within **seven days**[^1]. See [the terms](https://example.com/a%20b)\n\n## Steps\n\n1. Ask\n  - by mail\n2. Send\n\n| Item | Days |\n| --- | --- |\n| All \\| any |  |\n\n\\# not a heading\n\n[^1]: Seven calendar days.\n"
        );
    }

    #[test]
    fn a_file_that_is_not_word_is_refused() {
        let other = zip_of(&[("xl/workbook.xml", b"<workbook/>")]);
        assert!(matches!(
            convert(&other, &Limits::default()),
            Err(Refused::Unreadable(_))
        ));
        assert!(matches!(
            convert(b"not a zip", &Limits::default()),
            Err(Refused::Unreadable(_))
        ));
    }
}
