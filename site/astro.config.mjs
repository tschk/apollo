import { defineConfig } from "astro/config";

/** Static Astro output. Cloudflare Pages serves dist/ as-is. */
export default defineConfig({
  site: "https://apollo.tsc.hk",
  output: "static",
  build: {
    format: "directory",
  },
});
