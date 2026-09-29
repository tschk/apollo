import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { sampleFluid } from "../src/scripts/fluid";

describe("fluid field", () => {
  test("density stays inside 0..1 and moves with time", () => {
    const a = sampleFluid(1.2, 0.8, 0.2, 0.5, 0.4);
    const b = sampleFluid(1.2, 0.8, 3.4, 0.5, 0.4);
    expect(a).toBeGreaterThanOrEqual(0);
    expect(a).toBeLessThanOrEqual(1);
    expect(a).not.toBe(b);
  });
});

describe("built page", () => {
  const html = readFileSync(new URL("../dist/index.html", import.meta.url), "utf8");

  test("ships the launch lines and no stack jargon", () => {
    expect(html).toContain("The agent that sits on your computer.");
    expect(html).toContain("Short setup. Several instances. A simple chat, or the full app.");
    expect(html).toContain("/shots/advanced.png");
    expect(html).toContain("#09090b");
    for (const banned of ["local-first", "crepuscularity", "gpui", "rx4", "rotary", "cargo", "A desktop agent you open"]) {
      expect(html.toLowerCase()).not.toContain(banned.toLowerCase());
    }
  });
});
