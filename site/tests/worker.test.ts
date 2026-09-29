import { describe, expect, test } from "bun:test";
import { sampleFluid } from "../src/lib/fluid";
import worker from "../dist/worker.js";

describe("fluid field", () => {
  test("density stays inside 0..1 and moves with time", () => {
    const a = sampleFluid(1.2, 0.8, 0.2, 0.5, 0.4);
    const b = sampleFluid(1.2, 0.8, 3.4, 0.5, 0.4);
    expect(a).toBeGreaterThanOrEqual(0);
    expect(a).toBeLessThanOrEqual(1);
    expect(b).toBeGreaterThanOrEqual(0);
    expect(b).toBeLessThanOrEqual(1);
    expect(a).not.toBe(b);
  });
});

describe("worker", () => {
  const env = {
    ASSETS: { fetch: async () => new Response("missing", { status: 404 }) },
  };

  test("home sells the desktop app", async () => {
    const res = await worker.fetch(new Request("https://apollo.tsc.hk/"), env);
    expect(res.status).toBe(200);
    const html = await res.text();
    expect(html).toContain("The agent that sits on your computer.");
    expect(html).toContain("Short setup. Several instances. A simple chat, or the full app.");
    expect(html).not.toContain("A desktop agent you open");
    expect(html).toContain("#09090b");
    expect(html).toContain("/assets/app.css");
    expect(html).toContain("/shots/advanced.png");
    expect(html).not.toContain("local-first");
    expect(html).not.toContain("crepuscularity");
    expect(html).not.toContain("gpui");
    expect(html).not.toContain("rx4");
    expect(html).not.toContain("rotary");
    expect(html).not.toContain("cargo");
    expect(html).not.toContain("GEMINI_API_KEY=");
    expect(html).not.toContain("sk-");
  });

  test("unknown paths 404", async () => {
    const res = await worker.fetch(new Request("https://apollo.tsc.hk/nope"), env);
    expect(res.status).toBe(404);
    expect(await res.text()).toContain("not here");
  });
});
