// Prints the built files' gzipped sizes and fails past the budget (the console skill: under
// 150 KB of JavaScript and 30 KB of CSS, gzipped, for the console itself; the hall's scene, the
// three.js chunk loaded only in the hall view, under 200 KB on its own).
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { gzipSync } from "node:zlib";

const dir = new URL("../dist/assets/", import.meta.url);
const path = dir.pathname.replace(/^\/([A-Za-z]:)/, "$1");
const budget = { js: 150 * 1024, css: 30 * 1024, hall: 200 * 1024 };
const total = { js: 0, css: 0, hall: 0 };
for (const name of readdirSync(dir)) {
  const ext = name.split(".").pop();
  if (ext !== "js" && ext !== "css") continue;
  const size = gzipSync(readFileSync(join(path, name))).length;
  total[ext === "js" && name.startsWith("scene-") ? "hall" : ext] += size;
}
const label = { js: "js", css: "css", hall: "hall scene (js, on demand)" };
for (const k of ["js", "css", "hall"]) {
  const kb = (total[k] / 1024).toFixed(1);
  console.log(`${label[k]}: ${kb} KB gzipped (budget ${budget[k] / 1024} KB)`);
  if (total[k] > budget[k]) {
    console.error(`over the ${label[k]} budget`);
    process.exit(1);
  }
}
