<script lang="ts">
  import { cubicOut } from "svelte/easing";
  import { fly } from "svelte/transition";
  import { nav, parse } from "./nav.svelte";
  import Chat from "./screens/Chat.svelte";
  import NewAgent from "./screens/NewAgent.svelte";
  import Pool from "./screens/Pool.svelte";
  import Terminal from "./screens/Terminal.svelte";
  import Tree from "./screens/Tree.svelte";

  // The screens stack as a native app's do: the pool stays underneath,
  // scrolled where it was, and each deeper screen slides over it.
  let route = $derived(parse(nav.path));
  const slide = { x: window.innerWidth, duration: 280, easing: cubicOut, opacity: 1 };
</script>

<div class="stack">
  <div class="layer" inert={route.screen !== "pool"}><Pool /></div>
  {#if route.screen !== "pool" && route.tree !== null}
    {#key route.tree}
      <div class="layer over" transition:fly={slide} inert={route.screen !== "tree"}>
        <Tree name={route.tree} />
      </div>
    {/key}
  {/if}
  {#if route.screen === "chat" || route.screen === "terminal"}
    {#key route.tree}
      <div class="layer over" transition:fly={slide} inert={route.screen !== "chat"}>
        <Chat name={route.tree} />
      </div>
    {/key}
  {/if}
  {#if route.screen === "new"}
    <div class="layer over" transition:fly={{ ...slide, x: 0, y: window.innerHeight }}>
      <NewAgent tree={route.tree} />
    </div>
  {/if}
  {#if route.screen === "terminal"}
    {#key route.tree}
      <div class="layer over" transition:fly={slide}><Terminal name={route.tree} /></div>
    {/key}
  {/if}
</div>

<style>
  .stack {
    position: relative;
    height: 100%;
    overflow: hidden;
  }
  .layer {
    position: absolute;
    inset: 0;
  }
  .over {
    box-shadow: -8px 0 24px rgb(0 0 0 / 0.35);
  }
</style>
