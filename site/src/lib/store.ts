import { createSignal } from "@tschk/moonshine";

/** Visitor prefers reduced motion. Client-only; the worker never writes it. */
export const reducedMotion = createSignal(false);
