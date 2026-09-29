import { compile } from "svelte/compiler";

/** Compiles .svelte for Bun.build. Same recipe as accompany / moonshine svelte-adopt. */
export const sveltePlugin = (generate: "server" | "client") => ({
  name: "apollo-svelte",
  setup(build: {
    onLoad: (
      opts: { filter: RegExp },
      cb: (args: { path: string }) => Promise<{ contents: string; loader: "js" }>,
    ) => void;
  }) {
    build.onLoad({ filter: /\.svelte$/ }, async (args) => {
      const source = await Bun.file(args.path).text();
      const { js, css } = compile(source, {
        filename: args.path,
        generate,
        runes: true,
      });
      if (css?.code) {
        console.warn(`[svelte] ${args.path} has a <style> block — move it to app.css`);
      }
      return { contents: js.code, loader: "js" };
    });
  },
});
