// Screenshots of console pages, for looking at them while building (the console skill:
// light and dark, wide and phone-sized, reduced motion). Prints the browser console's errors
// and any Content-Security-Policy violation.
//
//   node scripts/shoot.mjs --base http://127.0.0.1:8091 --out shots --login root:1234 \
//     --routes overview,search?q=returns --themes light,dark --widths 1280,390 \
//     --layout hall --skies dawn,dusk,night --gpu 1
//
// `--layout list|hall` picks the console's layout; `--skies` shoots each route under each named
// sky (the hall's `?sky=`); `--gpu 1` asks Chromium for the real GPU, so the hall's shadows draw.
import { chromium, request } from "@playwright/test";
import { mkdirSync } from "node:fs";

const args = Object.fromEntries(
  process.argv
    .slice(2)
    .reduce((acc, a, i, all) => (a.startsWith("--") ? [...acc, [a.slice(2), all[i + 1] ?? ""]] : acc), []),
);
const base = args.base ?? "http://127.0.0.1:8091";
const out = args.out ?? "shots";
const routes = (args.routes ?? "overview").split(",");
const themes = (args.themes ?? "light").split(",");
const widths = (args.widths ?? "1280").split(",").map(Number);
const skies = args.skies ? args.skies.split(",") : [""];
const wait = Number(args.wait ?? 900);
mkdirSync(out, { recursive: true });

let token = "";
if (args.login) {
  const [user, password] = args.login.split(":");
  const api = await request.newContext({ baseURL: base });
  const resp = await api.post("/console/sign-in", { data: { user, password } });
  if (!resp.ok()) throw new Error(`signing in: ${resp.status()} ${await resp.text()}`);
  token = (await resp.json()).token;
  await api.dispose();
}

const browser = await chromium.launch({
  args: args.gpu === "1" ? ["--enable-gpu", "--ignore-gpu-blocklist", "--use-angle=d3d11"] : ["--enable-unsafe-swiftshader"],
});
let problems = 0;
for (const theme of themes) {
  for (const width of widths) {
    const context = await browser.newContext({
      viewport: { width, height: width < 600 ? 844 : Number(args.height ?? 900) },
      colorScheme: theme === "dark" ? "dark" : "light",
      reducedMotion: args.motion === "reduce" ? "reduce" : "no-preference",
      locale: args.lang === "en" ? "en-GB" : "zh-CN",
    });
    await context.addInitScript(
      ([t, layout]) => {
        if (t) localStorage.setItem("pinakes.console", JSON.stringify({ token: t, tenant: "", user: "", groups: "" }));
        if (layout) localStorage.setItem("pinakes.layout", layout);
      },
      [token, args.layout ?? ""],
    );
    const page = await context.newPage();
    page.on("console", (m) => {
      if (m.type() === "error" || m.type() === "warning") {
        problems++;
        console.log(`[${theme} ${width}] console ${m.type()}: ${m.text()}`);
      }
    });
    page.on("pageerror", (e) => {
      problems++;
      console.log(`[${theme} ${width}] page error: ${e.message}`);
    });
    for (const sky of skies) {
      for (const route of routes) {
        const [path, q = ""] = route.split("?");
        const query = new URLSearchParams(q);
        if (sky) query.set("sky", sky);
        const qs = query.toString();
        await page.goto(`${base}/ui/#/${path === "-" ? "" : path}${qs ? `?${qs}` : ""}`);
        await page.waitForTimeout(wait);
        const name = `${(path === "-" ? "hall" : path).replace(/[^a-z0-9]+/gi, "_")}${sky ? `-${sky}` : ""}-${theme}-${width}.png`;
        await page.screenshot({ path: `${out}/${name}`, fullPage: args.full === "1" });
        console.log(`${out}/${name}`);
      }
    }
    if (args.gpu === "1") {
      const gpu = await page.evaluate(() => {
        const gl = document.createElement("canvas").getContext("webgl2");
        const ext = gl?.getExtension("WEBGL_debug_renderer_info");
        return ext && gl ? String(gl.getParameter(ext.UNMASKED_RENDERER_WEBGL)) : "none";
      });
      console.log(`GPU: ${gpu}`);
    }
    await context.close();
  }
}
await browser.close();
if (problems) console.log(`${problems} problems`);
