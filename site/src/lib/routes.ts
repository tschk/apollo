import type { Component } from "svelte";
import Home from "../pages/Home.svelte";
import NotFound from "../pages/NotFound.svelte";

export type RouteMeta = {
  title: string;
  description: string;
  component: Component;
};

export const routes: Record<string, RouteMeta> = {
  "/": {
    title: "apollo — local-first agent host",
    description:
      "apollo is a local-first agent host. the gpui desktop app on crepuscularity: onboarding, instances, simple or advanced.",
    component: Home,
  },
};

export const notFound: RouteMeta = {
  title: "apollo — missing",
  description: "that page is not on apollo.tsc.hk.",
  component: NotFound,
};

export function resolveRoute(pathname: string): RouteMeta {
  const path = pathname.length > 1 ? pathname.replace(/\/+$/, "") : pathname;
  return routes[path] ?? notFound;
}

export function isKnownPath(pathname: string): boolean {
  const path = pathname.length > 1 ? pathname.replace(/\/+$/, "") : pathname;
  return path in routes;
}
