<script lang="ts">
  /** treetop's mark without its tile: a pine of four htop meter bars over a
   *  cursor-block trunk, as public/icons/icon.svg draws it. */
  let { size = 28 }: { size?: number } = $props();
</script>

<svg class="logo" viewBox="40 76 432 364" width={size} height={size} aria-hidden="true">
  <rect x="192" y="84" width="128" height="48" rx="24" fill="var(--yellow)" />
  <rect x="144" y="152" width="224" height="48" rx="24" fill="var(--green)" opacity=".28" />
  <rect x="144" y="152" width="160" height="48" rx="24" fill="var(--green)" />
  <rect x="96" y="220" width="320" height="48" rx="24" fill="var(--blue)" opacity=".28" />
  <rect x="96" y="220" width="152" height="48" rx="24" fill="var(--blue)" />
  <rect x="48" y="288" width="416" height="48" rx="24" fill="var(--green)" opacity=".28" />
  <rect x="48" y="288" width="344" height="48" rx="24" fill="var(--green)" />
  <rect x="228" y="356" width="56" height="76" rx="10" fill="var(--fg)" />
</svg>

<style>
  .logo {
    flex: none;
    display: block;
  }
</style>
