<script lang="ts">
  import { onMount } from "svelte";
  import Nav from "../components/Nav.svelte";
  import LetterSwap from "../components/LetterSwap.svelte";
  import { reducedMotion } from "../lib/store";

  let canvas: HTMLCanvasElement | undefined = $state();

  const providers = [
    "chatgpt",
    "claude",
    "gemini",
    "copilot",
    "mistral",
  ];

  onMount(() => {
    const media = window.matchMedia("(prefers-reduced-motion: reduce)");
    const apply = () => reducedMotion.set(media.matches);
    apply();
    media.addEventListener("change", apply);

    const cleanups: Array<() => void> = [];
    let disposed = false;

    if (canvas) {
      const node = canvas;
      void import("../lib/fluid").then(({ startFluid }) => {
        if (disposed) return;
        cleanups.push(startFluid(node, () => reducedMotion()));
      });
    }

    void import("lenis").then(({ default: Lenis }) => {
      if (disposed || media.matches) return;
      const lenis = new Lenis({ lerp: 0.09 });
      let frame = 0;
      const loop = (t: number) => {
        lenis.raf(t);
        frame = requestAnimationFrame(loop);
      };
      frame = requestAnimationFrame(loop);
      cleanups.push(() => {
        cancelAnimationFrame(frame);
        lenis.destroy();
      });
    });

    return () => {
      disposed = true;
      media.removeEventListener("change", apply);
      for (const cleanup of cleanups) cleanup();
    };
  });
</script>

<div class="site">
  <div class="sky" aria-hidden="true">
    <canvas bind:this={canvas} class="fluid pointer-events-none fixed inset-0 z-0 h-full w-full"></canvas>
    <div class="scrim"></div>
  </div>
  <Nav />
  <main>
    <section class="hero">
      <div class="wrap">
        <h1 class="word">apollo</h1>
        <p class="headline">The agent that sits on your computer.</p>
        <p class="subline">Short setup. Several instances. A simple chat, or the full app.</p>
        <div class="actions">
          <LetterSwap solid label="get apollo" href="#start" />
          <LetterSwap label="see the app" href="#app" />
        </div>
        <p class="meta-row">
          <span>mac</span>
          <span>linux</span>
          <span>tsc.hk</span>
        </p>
      </div>
    </section>

    <div class="ticker" aria-hidden="true">
      <div class="ticker-track">
        {#each [0, 1] as copy (copy)}
          {#each providers as name (`${copy}-${name}`)}
            <span>{name}</span>
          {/each}
        {/each}
      </div>
    </div>

    <section class="section" id="what">
      <div class="wrap">
        <div class="section-head">
          <span class="index">01</span>
          <h2>Leave it open. Come back to the same conversation.</h2>
        </div>
        <div class="grid-3">
          <article class="card">
            <h3>Where you already work</h3>
            <p>
              Apollo is an app on your computer. The thread stays there, not in a browser.
            </p>
          </article>
          <article class="card">
            <h3>A short setup</h3>
            <p>
              Connect a model, choose a folder, set the rules, and send one message to see it work.
            </p>
          </article>
          <article class="card">
            <h3>Quiet, or the whole app</h3>
            <p>
              Stay in the chat. Or open your instances, tools, and a record of what happened. Switch whenever you like.
            </p>
          </article>
        </div>
      </div>
    </section>

    <section class="section tight" id="setup">
      <div class="wrap grid-2">
        <div>
          <div class="section-head">
            <span class="index">02</span>
            <h2>Four screens, and you are in.</h2>
          </div>
          <ol class="steps">
            <li><span class="index">01</span><span><b>Connect a model.</b> Sign in, or paste a key you already have.</span></li>
            <li><span class="index">02</span><span><b>Point it somewhere.</b> One folder, or the whole computer.</span></li>
            <li><span class="index">03</span><span><b>Set the rules.</b> Say what it may do before it asks.</span></li>
            <li><span class="index">04</span><span><b>Try one message.</b> Then choose simple or advanced.</span></li>
          </ol>
        </div>
        <figure class="shot">
          <img src="/shots/welcome.png" alt="Apollo welcome screen with four setup steps and a get started button" />
          <figcaption><span>welcome</span><span>a few screens, then you are in</span></figcaption>
        </figure>
      </div>
    </section>

    <section class="section" id="app">
      <div class="wrap">
        <div class="section-head">
          <span class="index">03</span>
          <h2>Chat when you want quiet. Open the rest when you don't.</h2>
        </div>
        <div class="shots">
          <div class="pair">
            <figure class="shot">
              <img src="/shots/simple.png" alt="Simple mode: a conversation with the instance switcher and settings" />
              <figcaption><span>simple</span><span>the conversation</span></figcaption>
            </figure>
            <figure class="shot">
              <img src="/shots/ready.png" alt="Setup complete, choosing simple or advanced" />
              <figcaption><span>your choice</span><span>simple or advanced</span></figcaption>
            </figure>
          </div>
          <figure class="shot">
            <img src="/shots/advanced.png" alt="Advanced mode with a list of instances beside the conversation" />
            <figcaption><span>advanced</span><span>instances on the left, the conversation in the middle</span></figcaption>
          </figure>
          <div class="pair">
            <figure class="shot">
              <img src="/shots/instances.png" alt="Switching between two named instances, Apollo and Research" />
              <figcaption><span>instances</span><span>more than one agent, each with its own model</span></figcaption>
            </figure>
            <figure class="shot">
              <img src="/shots/tools.png" alt="Choosing how much the agent can do before it asks you" />
              <figcaption><span>permissions</span><span>you decide when it should ask</span></figcaption>
            </figure>
          </div>
        </div>
      </div>
    </section>

    <section class="section" id="start">
      <div class="wrap">
        <div class="section-head">
          <span class="index">04</span>
          <h2>Mac and Linux. Then the conversation.</h2>
        </div>
        <div class="install">
          <div class="card">
            <h3>Get Apollo</h3>
            <p>
              Install the latest release, open Apollo, and the setup is waiting.
            </p>
            <p class="fine" style="margin-top: 16px">
              <a href="https://github.com/tschk/apollo/releases/tag/v0.7.2">Get the release</a>
            </p>
          </div>
          <div class="card">
            <h3>What is inside</h3>
            <p>
              More than one instance, each with its own model. A simple chat, and an advanced view when you want tools and history.
            </p>
            <p class="fine" style="margin-top: 16px">
              <a href="https://github.com/tschk/apollo">Source</a>
              ·
              <a href="https://tsc.hk">tsc.hk</a>
            </p>
          </div>
        </div>
      </div>
    </section>
  </main>
  <div class="wrap">
    <footer>
      <span>Apollo</span>
      <span>no trackers · no cookies</span>
    </footer>
  </div>
</div>
