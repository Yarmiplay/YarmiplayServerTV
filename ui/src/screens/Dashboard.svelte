<script lang="ts">
  import { onMount } from "svelte";
  import type { Snapshot } from "../lib/api";
  import { api, formatDate, inApp } from "../lib/api";
  import { store } from "../lib/store.svelte";
  import { jellyfinPill, syncplayPill, tlsPill, updatedBy, updatePill, upnpPill } from "../lib/status";
  import Toggle from "../lib/Toggle.svelte";
  import Address from "../lib/Address.svelte";

  type Screen = "dashboard" | "syncplay" | "jellyfin" | "network" | "logs" | "about";
  let { snap, go }: { snap: Snapshot; go: (s: Screen) => void } = $props();

  let autostart = $state(false);
  onMount(async () => {
    autostart = await api.getAutostart().catch(() => false);
  });

  async function setAutostart(v: boolean) {
    await store.run(async () => {
      autostart = await api.setAutostart(v);
    });
  }

  const sp = $derived(syncplayPill(snap));
  const jf = $derived(jellyfinPill(snap));
  const tls = $derived(tlsPill(snap));
  const upnp = $derived(upnpPill(snap));
  const upd = $derived(updatePill(snap));
  const updateBusy = $derived(["checking", "downloading", "installing"].includes(snap.update.phase));
  const waiting = $derived(snap.syncplay.devices.pending.length);
  const needsJellyfinSetup = $derived(snap.jellyfin.phase === "running" && snap.jellyfin.wizardCompleted === false);
</script>

<div class="stack">
  <header>
    <h1>Dashboard</h1>
    <p class="muted">Host a Syncplay server and a Jellyfin server for YarmiplayTV and other Syncplay clients.</p>
  </header>

  <div class="grid2">
    <section class="card">
      <div class="spread">
        <h2>Syncplay</h2>
        <span class="pill {sp.kind}">{sp.text}</span>
      </div>
      <p class="sub">Keeps everyone's playback in sync.</p>
      <Toggle
        label="Syncplay server"
        hint={`Port ${snap.settings.syncplay.port}`}
        checked={snap.settings.syncplay.enabled}
        onchange={(v) => store.save((s) => (s.syncplay.enabled = v))}
      />
      {#if snap.syncplay.error}<div class="notice err">{snap.syncplay.error}</div>{/if}
      {#if waiting > 0}
        <div class="notice warn">
          {waiting === 1 ? "1 device is" : `${waiting} devices are`} waiting for approval.
        </div>
      {/if}
      {#if snap.settings.syncplay.enabled}
        <Address label="On your network" value={snap.addresses.syncplayLan} />
        <Address label="Over the internet" value={snap.addresses.syncplayPublic} />
      {/if}
      <div class="actions">
        <button class:primary={waiting > 0} onclick={() => go("syncplay")}>
          {waiting > 0 ? "Review devices" : "Syncplay settings"}
        </button>
      </div>
    </section>

    <section class="card">
      <div class="spread">
        <h2>Jellyfin</h2>
        <span class="pill {jf.kind}">{jf.text}</span>
      </div>
      <p class="sub">Your media library, streamed to YarmiplayTV.</p>
      <Toggle
        label="Jellyfin server"
        hint={snap.jellyfin.installedVersion ? `Version ${snap.jellyfin.installedVersion}` : `Downloads Jellyfin ${snap.jellyfin.pinnedVersion} when switched on`}
        checked={snap.settings.jellyfin.enabled}
        onchange={(v) => store.save((s) => (s.jellyfin.enabled = v))}
      />
      {#if snap.jellyfin.phase === "error" && snap.jellyfin.error}<div class="notice err">{snap.jellyfin.error}</div>{/if}
      {#if needsJellyfinSetup}
        <div class="notice warn">Jellyfin is running but not set up yet.</div>
      {/if}
      {#if snap.settings.jellyfin.enabled}
        <Address label="This PC" value={snap.addresses.jellyfinLocal} link />
        <Address label="Over the internet" value={snap.addresses.jellyfinPublic} />
      {/if}
      <div class="actions">
        <button class:primary={needsJellyfinSetup} onclick={() => go("jellyfin")}>
          {needsJellyfinSetup ? "Set up Jellyfin" : "Jellyfin settings"}
        </button>
      </div>
    </section>

    <section class="card">
      <div class="spread">
        <h2>Internet access</h2>
      </div>
      <p class="sub">Let friends outside your home network connect.</p>
      <div class="kv">
        <span>Encryption (DuckDNS)</span>
        <span class="pill {tls.kind}">{tls.text}</span>
        {#if snap.tls.host}
          <span>Domain</span><code>{snap.tls.host}</code>
          <span>Certificate expires</span><span>{formatDate(snap.tls.notAfter)}</span>
        {/if}
        <span>Port forwarding (UPnP)</span>
        <span class="pill {upnp.kind}">{upnp.text}</span>
        {#if snap.upnp.externalIp}<span>Public IP</span><code>{snap.upnp.externalIp}</code>{/if}
      </div>
      <div class="actions"><button onclick={() => go("network")}>Network settings</button></div>
    </section>

    <section class="card">
      <h2>App</h2>
      <p class="sub">Closing the window keeps the servers running in the tray.</p>
      <Toggle label="Start with system" hint="Starts minimized to the tray" checked={autostart} onchange={setAutostart} />
      <Toggle
        label="Browser access"
        hint={inApp
          ? "Open this control panel in a web browser on this PC"
          : "Turning this off disconnects this browser tab"}
        checked={snap.settings.browser.enabled}
        onchange={(v) => store.save((s) => (s.browser.enabled = v))}
      />
      <div class="actions">
        <button onclick={() => api.openFolder("data")}>Open data folder</button>
        {#if inApp && snap.settings.browser.enabled}
          <button onclick={() => store.run(api.openInBrowser)}>Open in browser</button>
        {/if}
      </div>
    </section>

    <section class="card">
      <div class="spread">
        <h2>Updates</h2>
        <span class="pill {upd.kind}">{upd.text}</span>
      </div>
      <p class="sub">
        {snap.update.managedBy
          ? `Version ${snap.version}. ${updatedBy(snap.update.managedBy)}.`
          : `You're running version ${snap.version}.`}
      </p>
      {#if snap.update.error}<div class="notice err">{snap.update.error}</div>{/if}
      {#if !snap.update.managedBy && snap.update.supported}
        <Toggle
          label="Automatic updates"
          hint={snap.update.unattended
            ? "Checks GitHub every few hours and installs new versions when nobody is using Syncplay"
            : "Checks GitHub every few hours and downloads new versions. This copy was installed with an .msi or .deb package, which asks for administrator permission, so installing waits for you"}
          checked={snap.settings.updates.auto}
          onchange={(v) => store.save((s) => (s.updates.auto = v))}
        />
        <div class="actions">
          {#if snap.update.phase === "ready"}
            <button class="primary" onclick={() => store.run(api.installUpdate)}>
              Install {snap.update.version} and restart
            </button>
          {:else}
            <button disabled={updateBusy} onclick={() => store.run(api.checkForUpdate)}>Check for updates</button>
          {/if}
        </div>
      {/if}
    </section>
  </div>
</div>

<style>
  header h1 {
    font-size: 1.6em;
  }
  header p {
    margin: 4px 0 0;
  }
  .actions {
    display: flex;
    gap: 8px;
    margin-top: 12px;
    flex-wrap: wrap;
  }
  .kv {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 8px 16px;
    align-items: center;
    justify-items: start;
    color: var(--muted);
    font-size: 0.93em;
  }
  .notice {
    margin: 6px 0;
  }
</style>
