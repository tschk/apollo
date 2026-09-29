import { hydrate } from "svelte";
import { isKnownPath, notFound, resolveRoute } from "./lib/routes";

const meta = isKnownPath(location.pathname) ? resolveRoute(location.pathname) : notFound;

hydrate(meta.component as never, {
  target: document.getElementById("app")!,
  props: {},
});
