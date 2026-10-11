import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

export default {
  preprocess: vitePreprocess(),
  compilerOptions: {
    runes: true,
    // Scoped class names from the CSS alone, not the file's path: the same build on every
    // system, so the committed dist/ can be checked against a fresh build anywhere.
    cssHash: ({ hash, css }) => `s-${hash(css)}`,
  },
};
