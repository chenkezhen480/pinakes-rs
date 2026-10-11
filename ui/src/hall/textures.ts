// The hall's surfaces, drawn with the canvas API: veined marble, cedar, limestone, papyrus, the
// mosaic floor with its meander border, the frieze, a coffered ceiling of lapis and gold stars,
// the inscribed stele, and soft glows for flames, the sun and clouds. Seeded, so the hall looks
// the same on every visit; nothing is downloaded.

/** A small seeded random source (mulberry32). */
export function seeded(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function canvas(w: number, h: number): [HTMLCanvasElement, CanvasRenderingContext2D] {
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  const g = c.getContext("2d");
  if (!g) throw new Error("no 2D canvas");
  return [c, g];
}

/** Fine speckle over a surface, for grain. */
function speckle(g: CanvasRenderingContext2D, w: number, h: number, n: number, alpha: number, rnd: () => number, light = false) {
  for (let i = 0; i < n; i++) {
    const v = light ? 255 : 0;
    g.fillStyle = `rgba(${v},${v},${v},${alpha * rnd()})`;
    g.fillRect(rnd() * w, rnd() * h, 1 + rnd() * 1.5, 1 + rnd() * 1.5);
  }
}

/** Marble with soft veins: `base` and `vein` as CSS colours. */
export function marble(base: string, vein: string, seed = 1, size = 512): HTMLCanvasElement {
  const [c, g] = canvas(size, size);
  const rnd = seeded(seed);
  g.fillStyle = base;
  g.fillRect(0, 0, size, size);
  // Clouded patches.
  for (let i = 0; i < 40; i++) {
    const x = rnd() * size;
    const y = rnd() * size;
    const r = 30 + rnd() * 120;
    const grad = g.createRadialGradient(x, y, 0, x, y, r);
    grad.addColorStop(0, `rgba(255,255,255,${0.05 + rnd() * 0.08})`);
    grad.addColorStop(1, "rgba(255,255,255,0)");
    g.fillStyle = grad;
    g.fillRect(x - r, y - r, r * 2, r * 2);
  }
  // Veins: wandering lines, wrapped so the texture tiles.
  g.strokeStyle = vein;
  g.lineCap = "round";
  for (let v = 0; v < 9; v++) {
    let x = rnd() * size;
    let y = rnd() * size;
    let a = rnd() * Math.PI * 2;
    g.globalAlpha = 0.12 + rnd() * 0.25;
    g.lineWidth = 0.6 + rnd() * 1.8;
    g.beginPath();
    g.moveTo(x, y);
    for (let s = 0; s < 120; s++) {
      a += (rnd() - 0.5) * 0.5;
      x += Math.cos(a) * 6;
      y += Math.sin(a) * 6;
      if (x < 0 || x > size || y < 0 || y > size) {
        x = (x + size) % size;
        y = (y + size) % size;
        g.stroke();
        g.beginPath();
        g.moveTo(x, y);
      } else g.lineTo(x, y);
    }
    g.stroke();
  }
  g.globalAlpha = 1;
  speckle(g, size, size, 3000, 0.05, rnd);
  return c;
}

/** Cedar: warm grain running along the texture's width. */
export function wood(seed = 2, size = 512, base = "#6b4226"): HTMLCanvasElement {
  const [c, g] = canvas(size, size);
  const rnd = seeded(seed);
  g.fillStyle = base;
  g.fillRect(0, 0, size, size);
  for (let y = 0; y < size; y += 2) {
    const dark = Math.sin(y * 0.09 + Math.sin(y * 0.013) * 4) * 0.5 + 0.5;
    g.fillStyle = `rgba(30,15,6,${0.06 + dark * 0.16})`;
    g.fillRect(0, y, size, 2);
  }
  g.strokeStyle = "rgba(25,12,5,0.35)";
  for (let i = 0; i < 70; i++) {
    const y0 = rnd() * size;
    g.lineWidth = 0.5 + rnd();
    g.beginPath();
    g.moveTo(0, y0);
    for (let x = 0; x <= size; x += 16) g.lineTo(x, y0 + Math.sin(x * 0.02 + i) * 3 * rnd());
    g.stroke();
  }
  // A few knots.
  for (let k = 0; k < 3; k++) {
    const x = rnd() * size;
    const y = rnd() * size;
    for (let r = 14; r > 2; r -= 3) {
      g.strokeStyle = `rgba(35,18,8,${0.25})`;
      g.beginPath();
      g.ellipse(x, y, r * 2.2, r, 0, 0, Math.PI * 2);
      g.stroke();
    }
  }
  speckle(g, size, size, 2500, 0.08, rnd, true);
  return c;
}

/** Limestone ashlar: courses of blocks with fine pitting. */
export function limestone(seed = 3, size = 512): HTMLCanvasElement {
  const [c, g] = canvas(size, size);
  const rnd = seeded(seed);
  g.fillStyle = "#cdbd9c";
  g.fillRect(0, 0, size, size);
  const rows = 4;
  const rh = size / rows;
  for (let r = 0; r < rows; r++) {
    const off = r % 2 === 0 ? 0 : size / 4;
    for (let b = -1; b < 3; b++) {
      const x = off + b * (size / 2);
      const tone = 190 + Math.floor(rnd() * 26);
      g.fillStyle = `rgb(${tone + 12},${tone},${tone - 30})`;
      g.fillRect(x + 2, r * rh + 2, size / 2 - 4, rh - 4);
    }
  }
  g.strokeStyle = "rgba(90,70,45,0.45)";
  g.lineWidth = 2;
  for (let r = 0; r <= rows; r++) {
    g.beginPath();
    g.moveTo(0, r * rh);
    g.lineTo(size, r * rh);
    g.stroke();
  }
  speckle(g, size, size, 9000, 0.12, rnd);
  speckle(g, size, size, 4000, 0.1, rnd, true);
  return c;
}

/** Papyrus: pale fibres crossing. */
export function papyrus(seed = 4, size = 256): HTMLCanvasElement {
  const [c, g] = canvas(size, size);
  const rnd = seeded(seed);
  g.fillStyle = "#e6d3a3";
  g.fillRect(0, 0, size, size);
  for (let i = 0; i < size; i += 3) {
    g.fillStyle = `rgba(150,110,50,${0.04 + rnd() * 0.08})`;
    g.fillRect(0, i, size, 1 + rnd() * 2);
    g.fillStyle = `rgba(150,110,50,${0.03 + rnd() * 0.05})`;
    g.fillRect(i, 0, 1 + rnd() * 2, size);
  }
  speckle(g, size, size, 1500, 0.08, rnd);
  return c;
}

/** Papyrus with a column of writing, for an unrolled scroll. */
export function writtenPapyrus(seed = 5): HTMLCanvasElement {
  const c = papyrus(seed, 512);
  const g = c.getContext("2d");
  if (!g) return c;
  const rnd = seeded(seed + 1);
  g.fillStyle = "rgba(40,28,18,0.75)";
  for (let col = 0; col < 3; col++) {
    const x0 = 40 + col * 160;
    for (let line = 0; line < 26; line++) {
      let x = x0;
      const y = 40 + line * 17;
      while (x < x0 + 120) {
        const w = 3 + rnd() * 10;
        g.fillRect(x, y, w, 2.2);
        x += w + 3;
      }
    }
  }
  return c;
}

/** The meander (Greek key) as a band: `fg` on `bg`. */
export function meander(fg: string, bg: string, w = 1024, h = 64): HTMLCanvasElement {
  const [c, g] = canvas(w, h);
  g.fillStyle = bg;
  g.fillRect(0, 0, w, h);
  g.strokeStyle = fg;
  g.lineWidth = h * 0.09;
  g.lineCap = "square";
  const u = h * 0.16;
  const step = u * 5;
  for (let x = 0; x < w; x += step) {
    const y = h * 0.82;
    g.beginPath();
    g.moveTo(x, y);
    g.lineTo(x + u * 4, y);
    g.lineTo(x + u * 4, y - u * 4);
    g.lineTo(x + u, y - u * 4);
    g.lineTo(x + u, y - u * 2);
    g.lineTo(x + u * 3, y - u * 2);
    g.lineTo(x + u * 3, y - u * 3);
    g.stroke();
  }
  g.fillStyle = fg;
  g.fillRect(0, h * 0.05, w, h * 0.06);
  g.fillRect(0, h * 0.9, w, h * 0.06);
  return c;
}

/** The floor: marble slabs, a lapis-and-ochre meander border down the aisle, and sixteen-point
 *  stars (the catalogue's compass) at the `stars` z positions. Drawn for the whole floor:
 *  `width` across, from z = `near` (the canvas's top) to z = `far`. */
export function floor(width: number, near: number, far: number, stars: number[], size = 2048): HTMLCanvasElement {
  const length = near - far;
  const w = size;
  const h = Math.round(size * (length / width));
  const [c, g] = canvas(w, h);
  const slab = marble("#e7dcc6", "#9a8466", 11, 512);
  const pat = g.createPattern(slab, "repeat");
  if (pat) g.fillStyle = pat;
  g.fillRect(0, 0, w, h);
  // Slab joints.
  const cell = w / (width / 2);
  g.strokeStyle = "rgba(110,90,62,0.35)";
  g.lineWidth = 2;
  for (let x = 0; x <= w; x += cell) {
    g.beginPath();
    g.moveTo(x, 0);
    g.lineTo(x, h);
    g.stroke();
  }
  for (let y = 0; y <= h; y += cell) {
    g.beginPath();
    g.moveTo(0, y);
    g.lineTo(w, y);
    g.stroke();
  }
  // The aisle: a long panel of darker stone down the middle, bordered by the meander.
  const ax = w * 0.27;
  const aw = w * 0.46;
  const ay = h * 0.06;
  const ah = h * 0.86;
  const dark = marble("#5d4a3a", "#c8b48e", 12, 512);
  const pd = g.createPattern(dark, "repeat");
  if (pd) g.fillStyle = pd;
  g.fillRect(ax, ay, aw, ah);
  const band = meander("#c99a3c", "#1e3d6b", 1024, 64);
  const bh = w * 0.035;
  const drawBand = (x: number, y: number, len: number, rot: number) => {
    g.save();
    g.translate(x, y);
    g.rotate(rot);
    for (let s = 0; s < len; s += bh * 16) g.drawImage(band, s, 0, Math.min(bh * 16, len - s), bh);
    g.restore();
  };
  drawBand(ax, ay - bh, aw, 0);
  drawBand(ax + aw, ay + ah + bh, aw, Math.PI);
  drawBand(ax - bh, ay + ah, ah, -Math.PI / 2);
  drawBand(ax + aw + bh, ay, ah, Math.PI / 2);
  // The star, under the armillary sphere and again under the catalogue table.
  const star = (cx: number, cy: number, r: number) => {
    g.save();
    g.translate(cx, cy);
    for (let ring = 0; ring < 3; ring++) {
      g.strokeStyle = ring === 1 ? "#c99a3c" : "#1e3d6b";
      g.lineWidth = r * 0.03;
      g.beginPath();
      g.arc(0, 0, r * (1.05 + ring * 0.08), 0, Math.PI * 2);
      g.stroke();
    }
    for (let i = 0; i < 16; i++) {
      const a = (i / 16) * Math.PI * 2;
      const long = i % 2 === 0 ? r : r * 0.62;
      g.fillStyle = i % 4 === 0 ? "#c99a3c" : i % 2 === 0 ? "#e7dcc6" : "#2b5486";
      g.beginPath();
      g.moveTo(0, 0);
      g.lineTo(Math.cos(a - 0.11) * r * 0.25, Math.sin(a - 0.11) * r * 0.25);
      g.lineTo(Math.cos(a) * long, Math.sin(a) * long);
      g.lineTo(Math.cos(a + 0.11) * r * 0.25, Math.sin(a + 0.11) * r * 0.25);
      g.closePath();
      g.fill();
    }
    g.restore();
  };
  const zToY = (z: number) => ((near - z) / length) * h;
  stars.forEach((z, i) => star(w / 2, zToY(z), w * (i === 0 ? 0.16 : 0.1)));
  return c;
}

/** The coffered ceiling: lapis coffers, each with a gold star, in gilded frames. */
export function coffers(size = 512): HTMLCanvasElement {
  const [c, g] = canvas(size, size);
  g.fillStyle = "#3d2a1c";
  g.fillRect(0, 0, size, size);
  const n = 2;
  const cell = size / n;
  for (let i = 0; i < n; i++) {
    for (let j = 0; j < n; j++) {
      const x = i * cell;
      const y = j * cell;
      const inset = cell * 0.1;
      g.fillStyle = "#8a6a3a";
      g.fillRect(x + inset * 0.5, y + inset * 0.5, cell - inset, cell - inset);
      g.fillStyle = "#1c3561";
      g.fillRect(x + inset, y + inset, cell - inset * 2, cell - inset * 2);
      // A five-pointed star.
      g.fillStyle = "#d9b25a";
      g.beginPath();
      const cx = x + cell / 2;
      const cy = y + cell / 2;
      for (let k = 0; k < 10; k++) {
        const r = k % 2 === 0 ? cell * 0.13 : cell * 0.052;
        const a = -Math.PI / 2 + (k * Math.PI) / 5;
        const px = cx + Math.cos(a) * r;
        const py = cy + Math.sin(a) * r;
        if (k === 0) g.moveTo(px, py);
        else g.lineTo(px, py);
      }
      g.closePath();
      g.fill();
    }
  }
  return c;
}

/** An inscribed stone: lines of capitals cut into marble. */
export function inscription(lines: string[], seed = 21): HTMLCanvasElement {
  const c = marble("#e9e1cf", "#a8957a", seed, 512);
  const g = c.getContext("2d");
  if (!g) return c;
  g.textAlign = "center";
  g.textBaseline = "middle";
  const sizes = [64, 30, 30, 30, 30];
  let y = 120;
  lines.forEach((line, i) => {
    const s = sizes[Math.min(i, sizes.length - 1)] ?? 30;
    g.font = `${i === 0 ? 700 : 500} ${s}px Georgia, "Times New Roman", serif`;
    // Cut letters: a dark core and a light lower edge.
    g.fillStyle = "rgba(255,255,255,0.6)";
    g.fillText(line, 256, y + 2);
    g.fillStyle = "#4a3a28";
    g.fillText(line, 256, y);
    y += i === 0 ? 90 : 52;
  });
  return c;
}

/** A clay or wax catalogue tablet with incised lines. */
export function tablet(seed = 31): HTMLCanvasElement {
  const [c, g] = canvas(256, 256);
  const rnd = seeded(seed);
  g.fillStyle = "#a27b4f";
  g.fillRect(0, 0, 256, 256);
  g.fillStyle = "#3a2a1c";
  g.fillRect(14, 14, 228, 228);
  g.fillStyle = "#8f6a42";
  g.fillRect(20, 20, 216, 216);
  g.strokeStyle = "rgba(40,25,12,0.8)";
  g.lineWidth = 2;
  for (let y = 40; y < 225; y += 16) {
    let x = 34;
    while (x < 220) {
      const w = 6 + rnd() * 18;
      g.beginPath();
      g.moveTo(x, y);
      g.lineTo(Math.min(x + w, 222), y);
      g.stroke();
      x += w + 6;
    }
  }
  return c;
}

/** A soft round glow: white at the centre to clear, for flames, the sun and the moon. */
export function glow(size = 128, inner = "rgba(255,255,255,1)", mid = "rgba(255,255,255,0.35)"): HTMLCanvasElement {
  const [c, g] = canvas(size, size);
  const r = size / 2;
  const grad = g.createRadialGradient(r, r, 0, r, r, r);
  grad.addColorStop(0, inner);
  grad.addColorStop(0.25, mid);
  grad.addColorStop(1, "rgba(255,255,255,0)");
  g.fillStyle = grad;
  g.fillRect(0, 0, size, size);
  return c;
}

/** A bank of cloud: many small soft lobes heaped along a flat base, thinning toward the ends,
 *  white (tinted by the material). */
export function cloud(seed = 41, w = 512, h = 192): HTMLCanvasElement {
  const [c, g] = canvas(w, h);
  const rnd = seeded(seed);
  for (let i = 0; i < 70; i++) {
    const t = rnd();
    const x = w * (0.08 + t * 0.84);
    // Taller in the middle, flat underneath.
    const lift = Math.sin(Math.PI * t) * (0.25 + rnd() * 0.3);
    const y = h * (0.7 - lift * rnd());
    const r = h * (0.07 + rnd() * 0.14) * (0.6 + Math.sin(Math.PI * t) * 0.6);
    const grad = g.createRadialGradient(x, y, 0, x, y, r);
    grad.addColorStop(0, "rgba(255,255,255,0.32)");
    grad.addColorStop(0.6, "rgba(255,255,255,0.12)");
    grad.addColorStop(1, "rgba(255,255,255,0)");
    g.fillStyle = grad;
    g.fillRect(x - r, y - r, r * 2, r * 2);
  }
  // Cut the base flat and soft.
  g.globalCompositeOperation = "destination-in";
  const base = g.createLinearGradient(0, 0, 0, h);
  base.addColorStop(0, "rgba(0,0,0,1)");
  base.addColorStop(0.72, "rgba(0,0,0,1)");
  base.addColorStop(0.9, "rgba(0,0,0,0)");
  g.fillStyle = base;
  g.fillRect(0, 0, w, h);
  return c;
}

/** A shaft of light: brightest at the top, fading down and to the sides. */
export function shaft(w = 64, h = 256): HTMLCanvasElement {
  const [c, g] = canvas(w, h);
  const v = g.createLinearGradient(0, 0, 0, h);
  v.addColorStop(0, "rgba(255,255,255,0.9)");
  v.addColorStop(1, "rgba(255,255,255,0)");
  g.fillStyle = v;
  g.fillRect(0, 0, w, h);
  g.globalCompositeOperation = "destination-in";
  const s = g.createLinearGradient(0, 0, w, 0);
  s.addColorStop(0, "rgba(0,0,0,0)");
  s.addColorStop(0.2, "rgba(0,0,0,1)");
  s.addColorStop(0.8, "rgba(0,0,0,1)");
  s.addColorStop(1, "rgba(0,0,0,0)");
  g.fillStyle = s;
  g.fillRect(0, 0, w, h);
  return c;
}

/** A fluted column: marble with twenty vertical channels around it. */
export function fluted(size = 512): HTMLCanvasElement {
  const c = marble("#efe7d6", "#a8977c", 7, size);
  const g = c.getContext("2d");
  if (!g) return c;
  const n = 20;
  const w = size / n;
  for (let i = 0; i < n; i++) {
    const grad = g.createLinearGradient(i * w, 0, (i + 1) * w, 0);
    grad.addColorStop(0, "rgba(70,55,35,0.28)");
    grad.addColorStop(0.5, "rgba(255,255,255,0.12)");
    grad.addColorStop(1, "rgba(70,55,35,0.28)");
    g.fillStyle = grad;
    g.fillRect(i * w, 0, w, size);
  }
  return c;
}

/** The flutes as relief: grey channels for a bump map (dark is deep). */
export function flutesBump(size = 256): HTMLCanvasElement {
  const [c, g] = canvas(size, 8);
  const n = 20;
  const w = size / n;
  for (let i = 0; i < n; i++) {
    const grad = g.createLinearGradient(i * w, 0, (i + 1) * w, 0);
    grad.addColorStop(0, "#ffffff");
    grad.addColorStop(0.12, "#707070");
    grad.addColorStop(0.5, "#202020");
    grad.addColorStop(0.88, "#707070");
    grad.addColorStop(1, "#ffffff");
    g.fillStyle = grad;
    g.fillRect(i * w, 0, w, 8);
  }
  return c;
}

/** A Pompeian wall: cinnabar panels between black bands, thin painted columns, green garlands,
 *  and a small vase or bird in the middle of each panel, over an ochre dado line. */
export function fresco(seed = 51, w = 2048, h = 512): HTMLCanvasElement {
  const [c, g] = canvas(w, h);
  const rnd = seeded(seed);
  g.fillStyle = "#1c1612";
  g.fillRect(0, 0, w, h);
  const panels = 6;
  const pw = w / panels;
  for (let p = 0; p < panels; p++) {
    const x = p * pw;
    // The panel, with a little uneven wash, so it reads as paint.
    g.fillStyle = "#8e2a1c";
    g.fillRect(x + pw * 0.12, h * 0.1, pw * 0.76, h * 0.72);
    for (let i = 0; i < 400; i++) {
      g.fillStyle = `rgba(${rnd() < 0.5 ? "40,10,5" : "200,90,60"},${0.04 * rnd()})`;
      const r = 4 + rnd() * 30;
      g.beginPath();
      g.arc(x + pw * (0.12 + rnd() * 0.76), h * (0.1 + rnd() * 0.72), r, 0, Math.PI * 2);
      g.fill();
    }
    // A thin ochre frame inside.
    g.strokeStyle = "#c99a3c";
    g.lineWidth = 3;
    g.strokeRect(x + pw * 0.16, h * 0.14, pw * 0.68, h * 0.64);
    // A garland swag across the top.
    g.strokeStyle = "#4f6b34";
    g.lineWidth = 9;
    g.beginPath();
    g.moveTo(x + pw * 0.16, h * 0.2);
    g.quadraticCurveTo(x + pw * 0.5, h * 0.42, x + pw * 0.84, h * 0.2);
    g.stroke();
    for (let k = 0; k < 18; k++) {
      const t = k / 17;
      const gx = x + pw * (0.16 + 0.68 * t);
      const gy = h * (0.2 + Math.sin(Math.PI * t) * 0.11);
      g.fillStyle = k % 3 === 0 ? "#c9a24a" : "#5d7d3c";
      g.beginPath();
      g.ellipse(gx, gy, 9, 6, t * 3, 0, Math.PI * 2);
      g.fill();
    }
    // In the middle: a vase, or a bird on a branch.
    const cx = x + pw / 2;
    const cy = h * 0.56;
    g.fillStyle = "#e3c58a";
    if (p % 2 === 0) {
      g.beginPath();
      g.moveTo(cx - 10, cy - 46);
      g.lineTo(cx + 10, cy - 46);
      g.quadraticCurveTo(cx + 40, cy - 10, cx + 16, cy + 30);
      g.lineTo(cx - 16, cy + 30);
      g.quadraticCurveTo(cx - 40, cy - 10, cx - 10, cy - 46);
      g.fill();
      g.fillRect(cx - 20, cy + 30, 40, 6);
    } else {
      g.strokeStyle = "#5d7d3c";
      g.lineWidth = 4;
      g.beginPath();
      g.moveTo(cx - 50, cy + 30);
      g.quadraticCurveTo(cx, cy + 10, cx + 50, cy + 26);
      g.stroke();
      g.beginPath();
      g.ellipse(cx, cy, 22, 13, -0.3, 0, Math.PI * 2);
      g.fill();
      g.beginPath();
      g.arc(cx + 20, cy - 12, 9, 0, Math.PI * 2);
      g.fill();
      g.beginPath();
      g.moveTo(cx - 18, cy + 2);
      g.lineTo(cx - 44, cy + 14);
      g.lineTo(cx - 16, cy + 10);
      g.fill();
    }
    // The painted column between panels.
    g.fillStyle = "#c99a3c";
    g.fillRect(x + pw - 5, h * 0.08, 10, h * 0.76);
    g.beginPath();
    g.arc(x + pw, h * 0.08, 14, 0, Math.PI * 2);
    g.fill();
  }
  // The dado line and the cornice line.
  g.fillStyle = "#c99a3c";
  g.fillRect(0, h * 0.86, w, 8);
  g.fillRect(0, h * 0.04, w, 5);
  g.fillStyle = "#2b211a";
  g.fillRect(0, h * 0.88, w, h * 0.12);
  speckle(g, w, h, 12000, 0.1, rnd);
  return c;
}

/** A black-figure band for an amphora: terracotta with a black frieze of running figures and a
 *  meander under it. */
export function amphora(seed = 61, w = 512, h = 256): HTMLCanvasElement {
  const [c, g] = canvas(w, h);
  const rnd = seeded(seed);
  g.fillStyle = "#b5602e";
  g.fillRect(0, 0, w, h);
  g.fillStyle = "#1a120c";
  g.fillRect(0, 0, w, h * 0.18);
  g.fillRect(0, h * 0.82, w, h * 0.18);
  // Running figures, simple and black.
  for (let i = 0; i < 6; i++) {
    const x = (i + 0.5) * (w / 6);
    const y = h * 0.5;
    g.beginPath();
    g.arc(x, y - 34, 9, 0, Math.PI * 2);
    g.fill();
    g.lineWidth = 7;
    g.strokeStyle = "#1a120c";
    g.beginPath();
    g.moveTo(x, y - 24);
    g.lineTo(x - 2, y + 6);
    g.lineTo(x - 16, y + 34);
    g.moveTo(x - 2, y + 6);
    g.lineTo(x + 14, y + 32);
    g.moveTo(x - 1, y - 14);
    g.lineTo(x + 18 * (rnd() < 0.5 ? 1 : -1), y - 6);
    g.stroke();
  }
  speckle(g, w, h, 1500, 0.1, rnd);
  return c;
}

/** Velvet: a deep colour with a soft woven grain and a woven border near the hem. */
export function velvet(base: string, border: string, w = 256, h = 512): HTMLCanvasElement {
  const [c, g] = canvas(w, h);
  const rnd = seeded(71);
  g.fillStyle = base;
  g.fillRect(0, 0, w, h);
  for (let y = 0; y < h; y += 2) {
    g.fillStyle = `rgba(0,0,0,${0.05 + rnd() * 0.05})`;
    g.fillRect(0, y, w, 1);
  }
  g.fillStyle = border;
  g.fillRect(0, h * 0.9, w, h * 0.02);
  g.fillRect(0, h * 0.95, w, h * 0.012);
  for (let x = 0; x < w; x += 24) {
    g.beginPath();
    g.moveTo(x, h * 0.925);
    g.lineTo(x + 12, h * 0.91);
    g.lineTo(x + 24, h * 0.925);
    g.lineTo(x + 12, h * 0.94);
    g.closePath();
    g.fill();
  }
  speckle(g, w, h, 2000, 0.08, rnd, true);
  return c;
}

/** A heap of cumulus: dense rounded lobes piled into a dome over a flat base, lit from above
 *  and greyer underneath (tinted by the material, so the sky's light colours it). */
export function cumulus(seed = 81, w = 512, h = 256): HTMLCanvasElement {
  const [c, g] = canvas(w, h);
  const rnd = seeded(seed);
  const base = h * 0.8;
  for (let i = 0; i < 90; i++) {
    const t = 0.06 + rnd() * 0.88;
    const dome = Math.pow(Math.sin(Math.PI * t), 0.8);
    const r = h * (0.07 + rnd() * 0.12) * (0.55 + dome * 0.75);
    const x = t * w;
    const y = base - r * 0.5 - dome * rnd() * h * 0.42;
    const grad = g.createRadialGradient(x, y - r * 0.2, 0, x, y, r);
    grad.addColorStop(0, "rgba(255,255,255,0.95)");
    grad.addColorStop(0.65, "rgba(255,255,255,0.8)");
    grad.addColorStop(1, "rgba(255,255,255,0)");
    g.fillStyle = grad;
    g.beginPath();
    g.arc(x, y, r, 0, Math.PI * 2);
    g.fill();
  }
  // Greyer toward the base, keeping the shape.
  g.globalCompositeOperation = "source-atop";
  const shade = g.createLinearGradient(0, h * 0.25, 0, base);
  shade.addColorStop(0, "rgba(120,130,150,0)");
  shade.addColorStop(1, "rgba(110,118,138,0.5)");
  g.fillStyle = shade;
  g.fillRect(0, 0, w, h);
  // A flat, soft base.
  g.globalCompositeOperation = "destination-in";
  const cut = g.createLinearGradient(0, 0, 0, h);
  cut.addColorStop(0, "rgba(0,0,0,1)");
  cut.addColorStop(0.78, "rgba(0,0,0,1)");
  cut.addColorStop(0.86, "rgba(0,0,0,0)");
  g.fillStyle = cut;
  g.fillRect(0, 0, w, h);
  return c;
}
