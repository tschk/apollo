# apollo site

**apollo.tsc.hk** — the public site for the apollo desktop app.

Svelte 5 (runes) on [moonshine](https://github.com/tschk/moonshine)'s signal
kernel, server-rendered in a Cloudflare Worker with an `ASSETS` binding and a
`custom_domain` route. Same deploy shape as accompany.tsc.hk, cupboard.tsc.hk,
and moonshine.tsc.hk: `wrangler deploy`, worker-first page routes.

Visual system is apollo's, not accompany's sky: zinc-950, Chivo Mono, lowercase.
The background is an original domain-warped fluid field drawn as ASCII glyphs
(the same kind of fluid-to-glyph field as undivisible.dev, not that page).
Letter-swap hovers follow fancycomponents' Letter Swap Forward and run through
the `motion` package. Lenis smooth-scrolls when motion is allowed.

Screenshots in `public/shots/` are the scrubbed v2 apollo-ui set. The api-key
screen is not included.

## Commands

```sh
cd site
bun install
bun run build
bun run preview     # http://127.0.0.1:4173
bun run dev         # wrangler dev on :8787
bun test
bun run typecheck
```

## Deploy

Do not run `bun run deploy` until you mean to attach the hostname. Wrangler is
configured for account `fc62a6e6528bec6d3d81c3bf8967ceeb` (the account that
holds the `tsc.hk` zone) and a `custom_domain` route for `apollo.tsc.hk`.
That deploy creates the worker `apollo-site` and the DNS record. It was not
run from this change: the checked-in oauth token cannot edit zone DNS.

```sh
cd site
CLOUDFLARE_ACCOUNT_ID=fc62a6e6528bec6d3d81c3bf8967ceeb bun run deploy
```

Needs a Cloudflare token on the Undivisible account with Workers Scripts and
Zone DNS Edit for zone `tsc.hk` (`0b4d96095d00ccbfb16a93d9f68b8328`). Do not
target the Twenifyscale account.
