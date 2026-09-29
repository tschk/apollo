<script lang="ts">
  import { onMount } from "svelte";
  import Nav from "../components/Nav.svelte";
  import LetterSwap from "../components/LetterSwap.svelte";
  import { reducedMotion } from "../lib/store";

  let canvas: HTMLCanvasElement | undefined = $state();
  let kicker = $state("desktop · crepuscularity + gpui");

  const providers = [
    "anthropic",
    "openai",
    "ollama",
    "openrouter",
    "groq",
    "mistral",
    "deepseek",
    "xai",
    "gemini",
    "copilot",
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

    const final = "desktop · crepuscularity + gpui";
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
          a local-first agent host. the desktop app is the desk: onboarding,
          instances, and a simple or advanced surface on crepuscularity and gpui.
        </p>
        <div class="actions">
          <LetterSwap solid label="read the source" href="https://github.com/tschk/apollo" />
          <LetterSwap label="how to install" href="#install" />
        </div>
        <p class="meta-row">
          <span>apollo-ui</span>
          <span>mpl-2.0</span>
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
          <h2>the agent lives here, not in a tab.</h2>
        </div>
        <div class="grid-3">
          <article class="card">
            <h3>local-first</h3>
            <p>
              one rust runtime on your computer. memory, sessions, and the http
              api stay on localhost. browsers are refused at the origin check,
              so a web page cannot drive the agent.
            </p>
          </article>
          <article class="card">
            <h3>a real desk</h3>
            <p>
              apollo-ui is the gpui app. a few screens: sign in or a key, pick
              a folder, set what it may do, run a test prompt, then open the desk.
            </p>
          </article>
          <article class="card">
            <h3>simple or advanced</h3>
            <p>
              simple is the chat and an instance switcher. advanced adds the
              roster, tools, permissions, and logs. switch any time in settings.
            </p>
          </article>
        </div>
      </div>
    </section>

    <section class="section tight" id="onboarding">
      <div class="wrap grid-2">
        <div>
          <div class="section-head">
            <span class="index">02</span>
            <h2>meet apollo, then get out of the way.</h2>
          </div>
          <ol class="steps">
            <li><span class="index">01</span><span><b>sign in, or a key.</b> an account, or a provider key kept in the instance env. owner-only. never the shared config.</span></li>
            <li><span class="index">02</span><span><b>one folder, or everywhere.</b> the workspace the agent is allowed to touch.</span></li>
            <li><span class="index">03</span><span><b>what it may do without asking.</b> permissions before the first real turn.</span></li>
            <li><span class="index">04</span><span><b>a test prompt.</b> then simple or advanced. everything below is on disk.</span></li>
          </ol>
        </div>
        <figure class="shot">
          <img src="/shots/welcome.png" alt="apollo-ui welcome: meet apollo, four setup steps, get started" />
          <figcaption><span>welcome</span><span>onboarding</span></figcaption>
        </figure>
      </div>
    </section>

    <section class="section" id="desk">
      <div class="wrap">
        <div class="section-head">
          <span class="index">03</span>
          <h2>two densities of the same desk.</h2>
        </div>
        <div class="shots">
          <div class="pair">
            <figure class="shot">
              <img src="/shots/simple.png" alt="simple mode: a single chat with the instance switcher and settings" />
              <figcaption><span>simple</span><span>chat, nothing else</span></figcaption>
            </figure>
            <figure class="shot">
              <img src="/shots/ready.png" alt="ready screen choosing simple or advanced after setup" />
              <figcaption><span>ready</span><span>how much app</span></figcaption>
            </figure>
          </div>
          <figure class="shot">
            <img src="/shots/advanced.png" alt="advanced mode with the instance roster, chat, and tools, logs, settings" />
            <figcaption><span>advanced</span><span>roster on the left, chat in the middle</span></figcaption>
          </figure>
          <div class="pair">
            <figure class="shot">
              <img src="/shots/instances.png" alt="instance switcher listing apollo and research" />
              <figcaption><span>instances</span><span>named desks, each with a model</span></figcaption>
            </figure>
            <figure class="shot">
              <img src="/shots/tools.png" alt="advanced tools: permission profiles auto, prompt, tools only, full" />
              <figcaption><span>tools</span><span>auto, prompt, tools only, full</span></figcaption>
            </figure>
          </div>
        </div>
      </div>
    </section>

    <section class="section" id="install">
      <div class="wrap">
        <div class="section-head">
          <span class="index">04</span>
          <h2>install the runtime. build the desk.</h2>
        </div>
        <div class="install">
          <pre><span class="prompt"># agent, from crates.io</span>
cargo install apollo-agent
apollo init
apollo ui

<span class="prompt"># desktop, from the checkout</span>
cargo run -p apollo-ui</pre>
          <div class="card">
            <h3>what you get</h3>
            <p>
              release binaries cover the agent on macos and linux.
              the gpui desk builds from the same repo. v0.7.2 is the current tag.
            </p>
            <p class="fine" style="margin-top: 16px">
              <a href="https://github.com/tschk/apollo/releases/tag/v0.7.2">github.com/tschk/apollo/releases</a>
            </p>
            <p class="fine" style="margin-top: 10px">
              <a href="https://tsc.hk">tsc.hk</a>
              ·
              <a href="https://github.com/tschk/crepuscularity">crepuscularity</a>
              ·
              <a href="https://github.com/tschk/rotary">rotary</a>
            </p>
          </div>
        </div>
      </div>
    </section>
  </main>
  <div class="wrap">
    <footer>
      <span>apollo · local-first agent host</span>
      <span>no trackers · no cookies</span>
    </footer>
  </div>
</div>
