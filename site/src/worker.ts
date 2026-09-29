import { render } from "svelte/server";
import { createRequestHandler } from "@tschk/moonshine-server";
import type { Renderer } from "@tschk/moonshine-framework";
import { isKnownPath, notFound, resolveRoute, routes } from "./lib/routes";

type Env = {
  ASSETS: { fetch: (request: Request) => Promise<Response> };
};

const SECURITY_HEADERS: Record<string, string> = {
  "content-security-policy":
    "default-src 'self'; img-src 'self' data:; style-src 'self'; font-src 'self'; script-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'",
  "x-content-type-options": "nosniff",
  "referrer-policy": "strict-origin-when-cross-origin",
  "permissions-policy": "camera=(), microphone=(), geolocation=()",
};

function page(pathname: string, status: number): Response {
  const meta = status === 404 ? notFound : resolveRoute(pathname);
  const out = render(meta.component as never, { props: {} });
  const canonical = "https://apollo.tsc.hk" + (pathname === "/404" ? "/" : pathname);

  const html = `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>${meta.title}</title>
<meta name="description" content="${meta.description}"/>
<meta property="og:title" content="${meta.title}"/>
<meta property="og:description" content="${meta.description}"/>
<meta property="og:type" content="website"/>
<meta property="og:url" content="${canonical}"/>
<meta name="theme-color" content="#09090b"/>
<link rel="icon" href="/favicon.svg" type="image/svg+xml"/>
<link rel="stylesheet" href="/assets/uno.css"/>
<link rel="stylesheet" href="/assets/app.css"/>
${out.head}
</head>
<body>
<div id="app">${out.body}</div>
<script type="module" src="/assets/client.js"></script>
</body>
</html>`;

  return new Response(html, {
    status,
    headers: {
      "content-type": "text/html; charset=utf-8",
      "cache-control": "public, max-age=0, must-revalidate",
      ...SECURITY_HEADERS,
    },
  });
}

const svelteRenderer: Renderer = {
  name: "svelte",
  async render({ route }) {
    return page(route.path, 200);
  },
  async prerender({ route }) {
    return page(route.path, 200).text();
  },
};

const handlePage = createRequestHandler({
  routes: Object.keys(routes).map((path) => ({
    id: path,
    path,
    file: path,
    mode: "ssr" as const,
    runtime: "cloudflare" as const,
  })),
  renderer: svelteRenderer,
  mode: "production",
  defaultMeta: { headers: SECURITY_HEADERS },
  notFound: ({ request }) => page(new URL(request.url).pathname, 404),
});

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const { pathname } = new URL(request.url);

    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response("method not allowed", { status: 405, headers: SECURITY_HEADERS });
    }

    if (isKnownPath(pathname)) return handlePage(request);

    const assetRes = await env.ASSETS.fetch(request);
    if (assetRes.status !== 404) return assetRes;

    return handlePage(request);
  },
};
