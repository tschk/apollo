<script lang="ts">
  import { onMount } from "svelte";
  import Nav from "../components/Nav.svelte";
  import LetterSwap from "../components/LetterSwap.svelte";
  import { reducedMotion } from "../lib/store";

  let canvas: HTMLCanvasElement | undefined = $state();
  let kicker = $state("desktop agent");

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

    const final = "desktop agent";
    let timer = 0;
    if (!media.matches) {
      const alphabet = "abcdefghijklmnopqrstuvwxyz.+";
      let step = 0;
      const total = 16;
      timer = window.setInterval(() => {
        step += 1;
        const keep = Math.floor((step / total) * final.length);
        kicker = final
          .split("")
          .map((ch, i) => {
            if (ch === " " || ch === "·" || i < keep) return ch;
            return alphabet[Math.floor(Math.random() * alphabet.length)] ?? ch;
          })
          .join("");
        if (step >= total) {
          kicker = final;
          window.clearInterval(timer);
        }
      }, 42);
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
      window.clearInterval(timer);
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
        <p class="kicker">{kicker}</p>
        <h1 class="word">apollo</h1>
        <p class="lede">
          A desktop agent you open, set up in a few screens, and talk to, with multiple instances and a simple or advanced mode.
        </p>
        <div class="actions">
          <LetterSwap solid label="get started" href="#start" />
          <LetterSwap label="view on github" href="https://github.com/tschk/apollo" />
        </div>
        <p class="meta-row">
          <span>mac and linux</span>
          <span>desktop app</span>
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
          <h2>An agent you open, not another browser tab.</h2>
        </div>
        <div class="grid-3">
          <article class="card">
            <h3>On your computer</h3>
            <p>
              Apollo is a desktop app. You open it, and the conversation stays on your machine.
            </p>
          </article>
          <article class="card">
            <h3>Ready in a few screens</h3>
            <p>
              Connect a model, choose what it can work on, decide when it should ask you, and send one test message.
            </p>
          </article>
          <article class="card">
            <h3>Simple or advanced</h3>
            <p>
              Simple is the chat. Advanced adds your instances, tools, and a record of what happened. Switch any time.
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
            <h2>Set up once. Then just talk.</h2>
          </div>
          <ol class="steps">
            <li><span class="index">01</span><span><b>Connect a model.</b> Sign in with an account you already have, or add a key.</span></li>
            <li><span class="index">02</span><span><b>Choose where it works.</b> One folder, or anywhere on your computer.</span></li>
            <li><span class="index">03</span><span><b>Decide when it should ask.</b> You set what it may do on its own.</span></li>
            <li><span class="index">04</span><span><b>Send a test message.</b> Then pick simple or advanced, and start.</span></li>
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
          <h2>Talk in simple. Open the full app when you want it.</h2>
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
          <h2>Open it on your Mac or Linux machine.</h2>
        </div>
        <div class="install">
          <div class="card">
            <h3>Get Apollo</h3>
            <p>
              The current release is on GitHub. Open Apollo, walk through setup, and start the conversation.
            </p>
            <p class="fine" style="margin-top: 16px">
              <a href="https://github.com/tschk/apollo/releases/tag/v0.7.2">Latest release</a>
            </p>
          </div>
          <div class="card">
            <h3>What you are opening</h3>
            <p>
              A desktop agent with multiple instances and two ways to use it: simple, for the chat, and advanced, for the full app.
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
      <span>Apollo · a desktop agent</span>
      <span>no trackers · no cookies</span>
    </footer>
  </div>
</div>
