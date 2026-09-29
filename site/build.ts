import { cpSync, mkdirSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { sveltePlugin } from "./src/lib/svelte-plugin";

function run(command: string, args: string[]): void {
  const result = spawnSync(command, args, { stdio: "inherit" });
  if (result.status !== 0) throw new Error(`${command} ${args.join(" ")} failed`);
}

rmSync("dist", { recursive: true, force: true });
mkdirSync("dist/public/assets", { recursive: true });
mkdirSync("dist/public/fonts", { recursive: true });

run("bunx", [
  "--no-install",
  "unocss",
  "src/**/*.{svelte,ts}",
  "--config",
  "uno.config.ts",
  "--preflights",
  "false",
  "--out-file",
  "dist/public/assets/uno.css",
]);

const client = await Bun.build({
  entrypoints: ["./src/client.ts"],
  outdir: "./dist/public/assets",
  target: "browser",
  format: "esm",
  minify: true,
  define: { "process.env.NODE_ENV": '"production"' },
  splitting: true,
  naming: { entry: "client.js", chunk: "[name]-[hash].js" },
  plugins: [sveltePlugin("client") as never],
});
if (!client.success) {
  console.error(client.logs);
  process.exit(1);
}

const worker = await Bun.build({
  entrypoints: ["./src/worker.ts"],
  outdir: "./dist",
  target: "bun",
  format: "esm",
  minify: true,
  define: { "process.env.NODE_ENV": '"production"' },
  naming: { entry: "worker.js" },
  plugins: [sveltePlugin("server") as never],
});
if (!worker.success) {
  console.error(worker.logs);
  process.exit(1);
}

cpSync("public", "dist/public", { recursive: true });
const fontDir = "node_modules/@fontsource/chivo-mono/files";
for (const name of [
  "chivo-mono-latin-400-normal.woff2",
  "chivo-mono-latin-500-normal.woff2",
]) {
  cpSync(`${fontDir}/${name}`, `dist/public/fonts/${name}`);
}
await Bun.write("dist/public/assets/app.css", Bun.file("src/app.css"));

const kb = (path: string) => (Bun.file(path).size / 1024) | 0;
console.log(
  `build ok — client ${kb("dist/public/assets/client.js")}kb, worker ${kb("dist/worker.js")}kb`,
);
