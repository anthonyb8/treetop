<script lang="ts">
  import { cubicOut } from "svelte/easing";
  import { fly } from "svelte/transition";
  import { nav, parse } from "./nav.svelte";
  import Pool from "./screens/Pool.svelte";
  import Tree from "./screens/Tree.svelte";

  // The screens stack as a native app's do: the pool stays underneath,
  // scrolled where it was, and each deeper screen slides over it.
  let route = $derived(parse(nav.path));
  const slide = { x: window.innerWidth, duration: 280, easing: cubicOut, opacity: 1 };
</script>

<div class="stack">
  <div class="layer" inert={route.screen !== "pool"}><Pool /></div>
  {#if route.screen === "tree"}
    {#key route.tree}
      <div class="layer over" transition:fly={slide}><Tree name={route.tree} /></div>
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
