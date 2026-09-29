<script lang="ts">
  import { onMount } from "svelte";

  /**
   * Letter Swap Forward from fancycomponents.dev, driven by the same
   * `motion` package accompany uses. Svelte, not the React island.
   */
  let {
    label,
    href,
    class: klass = "",
    solid = false,
  }: { label: string; href: string; class?: string; solid?: boolean } = $props();

  let root: HTMLAnchorElement | undefined = $state();
  const chars = $derived(Array.from(label));

  onMount(() => {
    const node = root;
    if (!node) return;
    let busy = false;
    const enter = async () => {
      if (busy || window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
      busy = true;
      const { animate, stagger } = await import("motion");
      const spring = { type: "spring" as const, duration: 0.55, bounce: 0 };
      await Promise.all([
        animate(node.querySelectorAll(".swap-a"), { y: "-100%" }, { ...spring, delay: stagger(0.02) }),
        animate(node.querySelectorAll(".swap-b"), { y: "-100%" }, { ...spring, delay: stagger(0.02) }),
      ]);
      animate(node.querySelectorAll(".swap-a"), { y: "0%" }, { duration: 0 });
      animate(node.querySelectorAll(".swap-b"), { y: "0%" }, { duration: 0 });
      busy = false;
    };
    node.addEventListener("pointerenter", enter);
    return () => node.removeEventListener("pointerenter", enter);
  });
</script>

<a bind:this={root} class="btn swap {klass}" class:solid href={href}>
  {#each chars as ch, i (i)}
    <span class="swap-char">
      <span class="swap-a">{ch === " " ? "\u00a0" : ch}</span>
      <span class="swap-b">{ch === " " ? "\u00a0" : ch}</span>
    </span>
  {/each}
</a>
