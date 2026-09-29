const root = new URL("../dist/", import.meta.url).pathname;
const port = Number(process.env.PORT ?? 4173);

Bun.serve({
  port,
  async fetch(request) {
    const url = new URL(request.url);
    let pathname = decodeURIComponent(url.pathname);
    if (pathname.endsWith("/")) pathname += "index.html";
    const file = Bun.file(root + pathname.slice(1));
    if (await file.exists()) return new Response(file);
    const missing = Bun.file(root + "404.html");
    if (await missing.exists()) return new Response(missing, { status: 404 });
    return new Response("not here", { status: 404 });
  },
});

console.log(`apollo site http://127.0.0.1:${port}`);
