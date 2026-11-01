<script lang="ts">
  import { fetchPushKey, subscribePush } from "../api";

  // Pushes reach an iPhone only in the app added to the home screen, where
  // Safari offers PushManager; elsewhere the bell does not show.
  const isSupported =
    typeof window !== "undefined" &&
    "serviceWorker" in navigator &&
    "PushManager" in window &&
    "Notification" in window;

  let status = $state<"off" | "on" | "blocked" | "working">(
    !isSupported ? "off" : Notification.permission === "denied" ? "blocked" : "off",
  );
  let failure = $state<string | null>(null);

  function bytes(base64url: string): Uint8Array<ArrayBuffer> {
    const base64 = base64url.replace(/-/g, "+").replace(/_/g, "/");
    const raw = atob(base64 + "=".repeat((4 - (base64.length % 4)) % 4));
    const out = new Uint8Array(new ArrayBuffer(raw.length));
    for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
    return out;
  }

  /** Subscribes this device, or tells the server again about the subscription it has. */
  async function subscribe(): Promise<void> {
    const key = await fetchPushKey();
    if (!key) throw new Error("This server sends no pushes");
    const registration = await navigator.serviceWorker.ready;
    const subscription =
      (await registration.pushManager.getSubscription()) ??
      (await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: bytes(key) }));
    await subscribePush(subscription.toJSON());
  }

  // Allowed before: make sure the server still has this device, as after a
  // reinstall of treetop or a subscription the browser renewed.
  $effect(() => {
    if (!isSupported || Notification.permission !== "granted") return;
    subscribe()
      .then(() => (status = "on"))
      .catch(() => (status = "off"));
  });

  async function turnOn() {
    failure = null;
    status = "working";
    try {
      const permission = await Notification.requestPermission();
      if (permission !== "granted") {
        status = permission === "denied" ? "blocked" : "off";
        return;
      }
      await subscribe();
      status = "on";
    } catch (err) {
      status = "off";
      failure = err instanceof Error ? err.message : "Notifications could not be turned on";
    }
  }
</script>

{#if isSupported && status !== "on"}
  <button
    type="button"
    class="bell"
    class:blocked={status === "blocked"}
    aria-label={status === "blocked" ? "Notifications are blocked in Settings" : "Turn on notifications"}
    title={failure ?? (status === "blocked" ? "Allow treetop in Settings › Notifications" : "Turn on notifications")}
    disabled={status === "working" || status === "blocked"}
    onclick={turnOn}
  >
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
      stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" /><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
      {#if status === "blocked"}<path d="M3 3l18 18" />{/if}
    </svg>
  </button>
{/if}

<style>
  .bell {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 44px;
    height: 44px;
    border-radius: 10px;
    border: 1px solid var(--yellow);
    background: var(--yellow-bg);
    color: var(--yellow);
  }
  .bell.blocked {
    border-color: var(--line-strong);
    background: var(--bg-high);
    color: var(--muted);
  }
</style>
