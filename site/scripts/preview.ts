import worker from "../dist/worker.js";

const root = new URL("../dist/public/", import.meta.url).pathname;
const port = Number(process.env.PORT ?? 4173);

Bun.serve({
  port,
  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname !== "/") {
      const file = Bun.file(root + url.pathname.slice(1));
      if (await file.exists()) return new Response(file);
    }
    return worker.fetch(request, {
      ASSETS: { fetch: async () => new Response(null, { status: 404 }) },
    });
  },
});

console.log(`apollo site http://127.0.0.1:${port}`);
