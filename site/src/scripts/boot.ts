import { createSignal } from "@tschk/moonshine";
import { animate, stagger } from "motion";
import Lenis from "lenis";
import { startFluid } from "./fluid";

/** Client entry. Moonshine owns the reduced-motion signal the fluid reads. */
export function boot(): void {
  const reduced = createSignal(
    window.matchMedia("(prefers-reduced-motion: reduce)").matches,
  );
  const media = window.matchMedia("(prefers-reduced-motion: reduce)");
  media.addEventListener("change", () => reduced.set(media.matches));

  const canvas = document.querySelector("canvas.fluid");
  if (canvas instanceof HTMLCanvasElement) {
    startFluid(canvas, () => reduced());
  }

  document.querySelectorAll("a.swap").forEach((node) => {
    let busy = false;
    node.addEventListener("pointerenter", async () => {
      if (busy || reduced()) return;
      busy = true;
      const spring = { type: "spring" as const, duration: 0.55, bounce: 0 };
      await Promise.all([
        animate(node.querySelectorAll(".swap-a"), { y: "-100%" }, { ...spring, delay: stagger(0.02) }),
        animate(node.querySelectorAll(".swap-b"), { y: "-100%" }, { ...spring, delay: stagger(0.02) }),
      ]);
      animate(node.querySelectorAll(".swap-a"), { y: "0%" }, { duration: 0 });
      animate(node.querySelectorAll(".swap-b"), { y: "0%" }, { duration: 0 });
      busy = false;
    });
  });

  if (!reduced()) {
    const lenis = new Lenis({ lerp: 0.09 });
    const loop = (time: number) => {
      lenis.raf(time);
      requestAnimationFrame(loop);
    };
    requestAnimationFrame(loop);
  }
}
