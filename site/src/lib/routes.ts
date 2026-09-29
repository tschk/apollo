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
    title: "Apollo — the agent on your computer",
    description:
      "The agent that sits on your computer. Short setup, several instances, a simple chat or the full app.",
    component: Home,
  },
};

export const notFound: RouteMeta = {
  title: "Apollo — page not found",
  description: "That page is not part of Apollo.",
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
