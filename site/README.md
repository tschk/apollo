# apollo site

**apollo.tsc.hk** — public site for the Apollo desktop app.

Astro (static) with client behavior on [Moonshine](https://github.com/tschk/moonshine)
signals. Chivo Mono, zinc-950, an ASCII fluid field, and the desktop screenshots.
Deployed to Cloudflare Pages: https://apollo-8e0.pages.dev (`wrangler pages deploy dist --project-name apollo`).

## Commands

```sh
cd site
bun install
bun run build
bun test
bun run preview     # http://127.0.0.1:4173
bun run deploy      # Pages project `apollo` on the Undivisible account
```

## Domain

`apollo.tsc.hk` is a CNAME to the Pages project. Deploy does not edit other
records on `tsc.hk`. If the token cannot write DNS, add:

| Type | Name | Content | Proxy |
| --- | --- | --- | --- |
| CNAME | apollo | `apollo-8e0.pages.dev` | Proxied |

Zone `tsc.hk` is `0b4d96095d00ccbfb16a93d9f68b8328` on account
`fc62a6e6528bec6d3d81c3bf8967ceeb`. Do not use the Twenifyscale account.
