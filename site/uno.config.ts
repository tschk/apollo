import { defineConfig, presetUno } from "unocss";

export default defineConfig({
  content: {
    filesystem: ["src/**/*.{svelte,ts}"],
  },
  preflights: [],
  presets: [presetUno()],
});
