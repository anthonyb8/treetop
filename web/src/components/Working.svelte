<script lang="ts">
  /** The agent at work: the dots a chat with Claude shows, and what it is doing now. */
  let { step }: { step: string | null } = $props();
</script>

<div class="working" role="status" aria-label="Claude is working{step ? `: ${step}` : ''}">
  <span class="dots" aria-hidden="true"><span></span><span></span><span></span></span>
  <span class="step">{step ?? "Working"}</span>
</div>

<style>
  .working {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
    padding: 4px 2px;
    color: var(--muted);
    font-size: 13px;
  }
  .dots {
    flex: none;
    display: flex;
    gap: 4px;
  }
  .dots span {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--orange);
    animation: bounce 1.2s ease-in-out infinite;
  }
  .dots span:nth-child(2) {
    animation-delay: 0.15s;
  }
  .dots span:nth-child(3) {
    animation-delay: 0.3s;
  }
  @keyframes bounce {
    0%,
    60%,
    100% {
      opacity: 0.35;
      transform: translateY(0);
    }
    30% {
      opacity: 1;
      transform: translateY(-4px);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dots span {
      animation: none;
      opacity: 0.7;
    }
  }
  .step {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
