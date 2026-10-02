<script lang="ts">
  import { onMount } from "svelte";
  import logo from "./assets/logo.svg";
  import { store } from "./lib/store.svelte";
  import { api, errorText } from "./lib/api";
  import { jellyfinPill, syncplayPill, tlsPill, upnpPill, type Pill } from "./lib/status";
  import Dashboard from "./screens/Dashboard.svelte";
  import Syncplay from "./screens/Syncplay.svelte";
  import Jellyfin from "./screens/Jellyfin.svelte";
  import Network from "./screens/Network.svelte";
  import Logs from "./screens/Logs.svelte";

  type Screen = "dashboard" | "syncplay" | "jellyfin" | "network" | "logs";
  let screen = $state<Screen>("dashboard");
  let loadError = $state<string | null>(null);

  onMount(() => {
    store.init().catch((e) => (loadError = errorText(e)));
  });

  const nav = $derived.by(() => {
    const s = store.snap;
    const pill = (f: (s: NonNullable<typeof store.snap>) => Pill) => (s ? f(s) : null);
    return [
      { id: "dashboard" as Screen, label: "Dashboard", pill: null },
      { id: "syncplay" as Screen, label: "Syncplay", pill: pill(syncplayPill) },
      { id: "jellyfin" as Screen, label: "Jellyfin", pill: pill(jellyfinPill) },
      {
        id: "network" as Screen,
        label: "Network",
        pill: s ? (s.settings.tls.enabled ? tlsPill(s) : upnpPill(s)) : null,
      },
      { id: "logs" as Screen, label: "Logs", pill: null },
    ];
  });
</script>

<div class="shell">
  <aside>
    <div class="brand">
      <img src={logo} alt="" width="40" height="40" />
      <div class="title">Yarmiplay<span>ServerTV</span></div>
    </div>
    <nav>
      {#each nav as item (item.id)}
        <button class:active={screen === item.id} onclick={() => (screen = item.id)}>
          <span>{item.label}</span>
          {#if item.pill && item.pill.kind}<span class="dot {item.pill.kind}" title={item.pill.text}></span>{/if}
        </button>
      {/each}
    </nav>
    <div class="foot">
      {#if store.snap}<span class="muted">v{store.snap.version}</span>{/if}
      <button class="small" onclick={() => api.quit()}>Quit</button>
    </div>
  </aside>

  <main>
    {#if loadError}
      <div class="notice err">Could not load the app state: {loadError}</div>
    {:else if !store.snap}
      <p class="muted">Loading…</p>
    {:else if screen === "dashboard"}
      <Dashboard snap={store.snap} go={(s: Screen) => (screen = s)} />
    {:else if screen === "syncplay"}
      <Syncplay snap={store.snap} />
    {:else if screen === "jellyfin"}
      <Jellyfin snap={store.snap} />
    {:else if screen === "network"}
      <Network snap={store.snap} />
    {:else}
      <Logs />
    {/if}
  </main>

  {#if store.toast}
    <div class="toast {store.toast.kind}" role="status">{store.toast.text}</div>
  {/if}
</div>

<style>
  .shell {
    display: grid;
    grid-template-columns: 220px 1fr;
    height: 100%;
  }
  aside {
    display: flex;
    flex-direction: column;
    gap: 18px;
    padding: 20px 14px;
    border-right: 1px solid var(--line);
    background: color-mix(in srgb, var(--card) 60%, var(--bg));
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 6px;
  }
  .title {
    font-weight: 800;
    font-size: 1.12em;
    letter-spacing: -0.01em;
  }
  .title span {
    color: var(--accent);
  }
  nav {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  nav button {
    display: flex;
    justify-content: space-between;
    align-items: center;
    text-align: left;
    background: transparent;
    border-color: transparent;
    color: var(--muted);
    padding: 9px 12px;
  }
  nav button.active {
    background: var(--card-high);
    color: var(--text);
    border-color: var(--line);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }
  .dot.ok {
    background: var(--ok);
  }
  .dot.warn {
    background: var(--warn);
  }
  .dot.err {
    background: var(--err);
  }
  .dot.busy {
    background: var(--accent);
  }
  .foot {
    margin-top: auto;
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0 6px;
    font-size: 0.88em;
  }
  main {
    overflow-y: auto;
    padding: 26px 30px 40px;
  }
  .toast {
    position: fixed;
    bottom: 20px;
    right: 24px;
    max-width: 460px;
    padding: 10px 16px;
    border-radius: 10px;
    background: var(--card-high);
    border: 1px solid var(--line);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
    user-select: text;
  }
  .toast.ok {
    border-color: color-mix(in srgb, var(--ok) 60%, transparent);
  }
  .toast.err {
    border-color: var(--err);
    color: var(--err);
  }
</style>
