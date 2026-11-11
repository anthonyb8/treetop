<script lang="ts">
  import { cubicOut } from "svelte/easing";
  import { fly } from "svelte/transition";
  import PortToast from "./components/PortToast.svelte";
  import TabBar from "./components/TabBar.svelte";
  import { watchWaiting } from "./waiting.svelte";
  import { nav, parse } from "./nav.svelte";
  import Chat from "./screens/Chat.svelte";
  import Issue from "./screens/Issue.svelte";
  import IssueForm from "./screens/IssueForm.svelte";
  import Issues from "./screens/Issues.svelte";
  import NewAgent from "./screens/NewAgent.svelte";
  import Pool from "./screens/Pool.svelte";
  import Tree from "./screens/Tree.svelte";
  import Waiting from "./screens/Waiting.svelte";

  // The screens stack as a native app's do: the pool and the issues stay
  // underneath, scrolled where they were, and each deeper screen slides over
  // them.
  let route = $derived(parse(nav.path));
  const slide = { x: window.innerWidth, duration: 280, easing: cubicOut, opacity: 1 };

  let isIssueRoute = $derived(route.screen === "issues" || route.screen === "issue" || route.screen === "issue-form");
  // A tree opened from an issue, or from what waits, keeps that tab
  // underneath it.
  let tab = $state<"trees" | "waiting" | "issues">("trees");
  let hasOpenedIssues = $state(false);
  $effect(() => {
    if (route.screen === "pool") tab = "trees";
    if (route.screen === "waiting") tab = "waiting";
    if (isIssueRoute) {
      tab = "issues";
      hasOpenedIssues = true;
    }
  });
  let isRoot = $derived(route.screen === "pool" || route.screen === "waiting" || route.screen === "issues");
  // What waits is asked for on every screen, so its tab's count is current.
  $effect(() => watchWaiting());
  let tree = $derived(
    route.screen === "tree" || route.screen === "chat" || (route.screen === "new" && route.tree !== null)
      ? route.tree
      : null,
  );
</script>

<div class="stack">
  <div class="layer root" inert={!isRoot}>
    <div class="pages">
      <div class="page" class:shown={tab === "trees"}><Pool /></div>
      <div class="page" class:shown={tab === "waiting"}><Waiting /></div>
      {#if hasOpenedIssues}
        <div class="page" class:shown={tab === "issues"}><Issues isActive={route.screen === "issues"} /></div>
      {/if}
    </div>
    <TabBar {tab} />
  </div>
  {#if route.screen === "issue"}
    {#key route.id}
      <div class="layer over" transition:fly={slide}>
        <Issue id={route.id} />
      </div>
    {/key}
  {/if}
  {#if route.screen === "issue-form"}
    <div class="layer over" transition:fly={{ ...slide, x: 0, y: window.innerHeight }}>
      <IssueForm id={route.id} />
    </div>
  {/if}
  {#if tree !== null}
    {#key tree}
      <div class="layer over" transition:fly={slide} inert={route.screen !== "tree"}>
        <Tree name={tree} />
      </div>
    {/key}
  {/if}
  {#if route.screen === "chat"}
    {#key route.tree}
      <div class="layer over" transition:fly={slide}><Chat name={route.tree} /></div>
    {/key}
  {/if}
  {#if route.screen === "new"}
    <div class="layer over" transition:fly={{ ...slide, x: 0, y: window.innerHeight }}>
      <NewAgent tree={route.tree} />
    </div>
  {/if}
  <PortToast />
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
  .root {
    display: flex;
    flex-direction: column;
  }
  .pages {
    position: relative;
    flex: 1;
    min-height: 0;
  }
  .page {
    position: absolute;
    inset: 0;
    display: none;
  }
  .page.shown {
    display: block;
  }
  .over {
    box-shadow: -8px 0 24px rgb(0 0 0 / 0.35);
  }
</style>
