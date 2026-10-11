// A line diff (longest common subsequence), for showing a held edit against the page it
// changes. Pages are short; past a few thousand lines it falls back to "all removed, all added".

export type DiffLine = { op: "same" | "add" | "del"; text: string };

export function diffLines(before: string, after: string): DiffLine[] {
  const a = before.split("\n");
  const b = after.split("\n");
  if (a.length * b.length > 4_000_000) {
    return [...a.map((text) => ({ op: "del" as const, text })), ...b.map((text) => ({ op: "add" as const, text }))];
  }
  const n = a.length;
  const m = b.length;
  // lcs[i][j]: the common length of a[i..] and b[j..], in one flat array.
  const lcs = new Uint32Array((n + 1) * (m + 1));
  const at = (i: number, j: number) => i * (m + 1) + j;
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      lcs[at(i, j)] = a[i] === b[j] ? (lcs[at(i + 1, j + 1)] ?? 0) + 1 : Math.max(lcs[at(i + 1, j)] ?? 0, lcs[at(i, j + 1)] ?? 0);
    }
  }
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      out.push({ op: "same", text: a[i] ?? "" });
      i++;
      j++;
    } else if ((lcs[at(i + 1, j)] ?? 0) >= (lcs[at(i, j + 1)] ?? 0)) {
      out.push({ op: "del", text: a[i] ?? "" });
      i++;
    } else {
      out.push({ op: "add", text: b[j] ?? "" });
      j++;
    }
  }
  while (i < n) out.push({ op: "del", text: a[i++] ?? "" });
  while (j < m) out.push({ op: "add", text: b[j++] ?? "" });
  return out;
}

/** The diff with long unchanged stretches folded to a few lines of context. */
export function folded(lines: DiffLine[], context = 2): (DiffLine | { op: "fold"; count: number })[] {
  const keep = lines.map((l, i) =>
    lines.slice(Math.max(0, i - context), i + context + 1).some((x) => x.op !== "same"),
  );
  const out: (DiffLine | { op: "fold"; count: number })[] = [];
  let skipped = 0;
  lines.forEach((l, i) => {
    if (keep[i]) {
      if (skipped) out.push({ op: "fold", count: skipped });
      skipped = 0;
      out.push(l);
    } else skipped++;
  });
  if (skipped) out.push({ op: "fold", count: skipped });
  return out;
}
