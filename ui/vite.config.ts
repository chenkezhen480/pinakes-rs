import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import process from "node:process";

// The API's top-level paths, proxied by `npm run dev` to a service on 127.0.0.1:8080, or the
// one PINAKES_URL names.
const target = process.env.PINAKES_URL ?? "http://127.0.0.1:8080";
const api = [
  "/status", "/pages", "/history", "/search", "/read", "/graph", "/ingest", "/jobs",
  "/proposals", "/acquisition", "/lint", "/admin", "/users", "/console", "/health", "/dev",
];

export default defineConfig({
  base: "/ui/",
  plugins: [svelte()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // One script and one style sheet, and the hall's scene (three.js) as a chunk of its own,
    // loaded only in the hall view; nothing inline (the service's Content-Security-Policy allows
    // only its own files). The budgets are checked by scripts/sizes.mjs.
    assetsInlineLimit: 0,
    chunkSizeWarningLimit: 700,
    modulePreload: { polyfill: false },
    target: "es2022",
  },
  server: {
    host: "127.0.0.1",
    proxy: Object.fromEntries(api.map((p) => [p, { target }])),
  },
});
