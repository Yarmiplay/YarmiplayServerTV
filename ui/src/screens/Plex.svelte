<script lang="ts">
  import type { PlexLibrary, Snapshot } from "../lib/api";
  import { api, errorText, formatDate } from "../lib/api";
  import { store } from "../lib/store.svelte";
  import { useDraft } from "../lib/draft.svelte";
  import { plexPill } from "../lib/status";
  import Toggle from "../lib/Toggle.svelte";
  import Address from "../lib/Address.svelte";
  import Libraries from "../lib/Libraries.svelte";

  let { snap }: { snap: Snapshot } = $props();

  const DOWNLOAD_URL = "https://www.plex.tv/media-server-downloads/";

  const p = $derived(snap.plex);
  const acct = $derived(snap.plexAccount);
  const settings = $derived(snap.settings.plex);
  const pill = $derived(plexPill(snap));
  const running = $derived(p.phase === "running");
  const notOwned = $derived(running && acct.signedIn && acct.ownsServer === false);
  const ready = $derived(running && acct.signedIn && acct.ownsServer === true);
  const vanilla = $derived(snap.settings.syncplay.vanillaMode);
  const sharing = $derived(settings.shareWithSyncplay && !vanilla);

  const os = navigator.userAgent.includes("Windows") ? "windows" : navigator.userAgent.includes("Mac") ? "mac" : "linux";
  const inSnap = $derived(snap.update.managedBy === "snap");

  const port = useDraft(() => ({ port: settings.port }));

  // Libraries
  let libraries = $state<PlexLibrary[] | null>(null);
  let libError = $state<string | null>(null);

  async function loadLibraries() {
    libError = null;
    try {
      libraries = await api.plexLibraries();
    } catch (e) {
      libError = errorText(e);
    }
  }

  $effect(() => {
    if (ready && libraries === null) loadLibraries();
    if (!ready) libraries = null;
  });

  async function addLibrary(name: string, kind: string, path: string): Promise<boolean> {
    const ok = await store.run(() => api.plexAddLibrary(name, kind, path), `Added ${name}`);
    await loadLibraries();
    return ok !== undefined;
  }

  async function change(call: () => Promise<void>, done: string) {
    await store.run(call, done);
    await loadLibraries();
  }

  const kinds: [string, string][] = [
    ["movies", "Movies"],
    ["shows", "TV shows"],
    ["music", "Music"],
    ["photos", "Photos"],
    ["other", "Other videos"],
  ];
  const kindLabel = (k: string) =>
    ({ movie: "Movies", show: "TV shows", artist: "Music", photo: "Photos" })[k] ?? "Other";

  // Which libraries Syncplay users get (null = all).
  const isShared = (key: string) => settings.sharedSections === null || settings.sharedSections.includes(key);
  function setShared(key: string, on: boolean) {
    const all = (libraries ?? []).map((l) => l.key);
    const current = settings.sharedSections ?? all;
    const next = on ? [...new Set([...current, key])] : current.filter((k) => k !== key);
    const everything = all.every((k) => next.includes(k));
    store.save((s) => (s.plex.sharedSections = everything ? null : next));
  }

  const linkMinutes = $derived(
    acct.link ? Math.max(1, Math.round((acct.link.expiresAt * 1000 - Date.now()) / 60000)) : 0,
  );
</script>

<div class="stack">
  <header class="spread">
    <h1>Plex</h1>
    <span class="pill {pill.kind}">{pill.text}</span>
  </header>

  <section class="card">
    <Toggle
      label="Plex Media Server"
      hint="Use the Plex Media Server installed on this PC: manage its libraries here and share it with your Syncplay users. Plex itself is installed and run by you."
      checked={settings.enabled}
      onchange={(v) => store.save((s) => (s.plex.enabled = v))}
    />

    {#if settings.enabled}
      {#if p.phase === "notInstalled"}
        <div class="notice warn guide">
          <strong>Install Plex Media Server</strong>
          <p>Download it from plex.tv, install it and start it. This page picks it up on its own.</p>
          <div class="row"><button class="small primary" onclick={() => api.openUrl(DOWNLOAD_URL)}>Download Plex</button></div>
        </div>
      {:else if p.phase === "notRunning"}
        <div class="notice warn guide">
          <strong>{inSnap ? "Plex isn't reachable" : "Plex isn't running"}</strong>
          {#if os === "windows"}
            <p>Start <em>Plex Media Server</em> from the Start menu. Not installed yet? Get it from plex.tv.</p>
          {:else if os === "mac"}
            <p>Open <em>Plex Media Server</em> from your Applications folder. Not installed yet? Get it from plex.tv.</p>
          {:else}
            <p>
              Start it with <code>sudo systemctl start plexmediaserver</code>. Not installed yet? Get the package for your
              distribution from plex.tv.
            </p>
          {/if}
          {#if settings.port !== 32400}<p class="muted">Looking for Plex on port {settings.port}.</p>{/if}
          <div class="row">
            <button class="small" onclick={() => api.plexRefresh()}>Check again</button>
            <button class="small" onclick={() => api.openUrl(DOWNLOAD_URL)}>Download Plex</button>
          </div>
        </div>
      {:else if p.phase === "unclaimed"}
        <div class="notice warn guide">
          <strong>Finish setting up Plex</strong>
          <p>Plex is running but not signed in to a Plex account yet. Sign in and name your server in Plex's own setup.</p>
          <div class="row">
            <button class="small primary" onclick={() => snap.addresses.plexLocal && api.openUrl(snap.addresses.plexLocal)}>
              Finish setup in Plex
            </button>
            <button class="small" onclick={() => api.plexRefresh()}>Check again</button>
          </div>
        </div>
      {/if}

      {#if running || p.phase === "unclaimed"}
        <div class="addrs">
          <Address label="This PC" value={snap.addresses.plexLocal} link />
          <Address label="On your network" value={snap.addresses.plexLan} />
          <Address label="Over the internet" value={snap.addresses.plexPublic} />
        </div>
        {#if p.version}<p class="muted note">{p.serverName ? `${p.serverName} · ` : ""}Plex Media Server {p.version}</p>{/if}
      {/if}
    {/if}
  </section>

  {#if settings.enabled && running}
    {#if !acct.signedIn}
      <section class="card setup">
        <h2>Link your Plex account</h2>
        <p class="sub">
          Link the Plex account this server belongs to, so you can manage libraries and sharing from here. You sign in on
          plex.tv; this app never sees your password.
        </p>
        {#if acct.link}
          <p>On your phone or PC, go to <strong>plex.tv/link</strong> and enter this code:</p>
          <div class="code selectable">{acct.link.code}</div>
          <p class="muted note">Waiting for you to enter it… The code works for about {linkMinutes} more minute{linkMinutes === 1 ? "" : "s"}.</p>
          <div class="row buttons">
            <button class="primary" onclick={() => api.openUrl("https://plex.tv/link")}>Open plex.tv/link</button>
            <button onclick={async () => (store.snap = await api.plexLinkCancel())}>Cancel</button>
          </div>
        {:else}
          {#if acct.error}<div class="notice err">{acct.error}</div>{/if}
          <div class="row buttons">
            <button class="primary" onclick={() => store.run(async () => (store.snap = await api.plexLinkStart()))}>Get a link code</button>
          </div>
        {/if}
      </section>
    {:else if notOwned}
      <section class="card">
        <h2>Plex account</h2>
        <div class="notice err">
          This Plex server belongs to another plex.tv account than <strong>{settings.accountUser}</strong>. Link the account
          that owns it.
        </div>
        <div class="row buttons">
          <button onclick={async () => (store.snap = await api.plexLogout())}>Sign out</button>
          <button onclick={() => api.plexRefresh()}>Check again</button>
        </div>
      </section>
    {:else if ready}
      <section class="card">
        <div class="spread">
          <div>
            <h2>Libraries</h2>
            <p class="sub">
              Linked to <strong>{settings.accountUser}</strong>{p.serverName ? ` on ${p.serverName}` : ""}.
            </p>
          </div>
          <div class="row">
            <button class="small" onclick={() => store.run(() => api.plexRescan(), "Scanning libraries")}>Scan all</button>
            <button class="small" onclick={async () => (store.snap = await api.plexLogout())}>Sign out</button>
          </div>
        </div>
        {#if libError}<div class="notice err">{libError}</div>{/if}
        <Libraries
          libraries={libraries?.map((l) => ({ id: l.key, name: l.name, kind: l.kind, locations: l.locations })) ?? null}
          {kinds}
          {kindLabel}
          folderNote={os === "linux"
            ? "Plex runs as its own user (plex), which must be able to read the folder."
            : "Plex must be able to read this folder."}
          onAdd={addLibrary}
          onAddPath={(key, path) => change(() => api.plexAddPath(key, path), "Folder added")}
          onRemovePath={(key, path) => change(() => api.plexRemovePath(key, path), "Folder removed")}
          onRemove={(key, name) => change(() => api.plexRemoveLibrary(key), `Removed ${name}`)}
        />
      </section>
    {:else}
      <section class="card"><p class="muted">Checking your Plex account…</p></section>
    {/if}
  {/if}

  {#if settings.enabled}
    <section class="card">
      <Toggle
        label="Share with Syncplay users"
        hint={vanilla
          ? "Off in vanilla Syncplay mode"
          : "People on your Syncplay server can ask for your Plex in YarmiplayTV. Each one gets a Plex friend invite to the libraries you pick, which they accept with their own Plex account."}
        disabled={vanilla}
        checked={sharing}
        onchange={(v) => store.save((s) => (s.plex.shareWithSyncplay = v))}
      />
      {#if sharing}
        {#if snap.settings.syncplay.access === "open"}
          <div class="notice warn">
            Anyone can join the Syncplay server, so anyone who finds it can ask for your Plex. On the Syncplay page, choose a
            password option or approved devices only under "Who can join".
          </div>
        {/if}
        {#if snap.plexShare.error}<div class="notice err">{snap.plexShare.error}</div>{/if}
        <p class="muted note">
          {#if snap.plexShare.active}
            Shared. Turning this off removes the invites made from here; friends you added in Plex keep their access.
          {:else if !snap.settings.syncplay.enabled}
            Turn on the Syncplay server to share Plex with its users.
          {:else if !ready}
            Sharing starts once Plex is running and your Plex account is linked here.
          {:else}
            Setting up sharing…
          {/if}
        </p>

        {#if ready && libraries && libraries.length > 0}
          <h3>Shared libraries</h3>
          <div class="checks">
            {#each libraries as lib (lib.key)}
              <label class="check">
                <input type="checkbox" checked={isShared(lib.key)} onchange={(e) => setShared(lib.key, e.currentTarget.checked)} />
                {lib.name}
              </label>
            {/each}
          </div>
        {/if}
      {/if}

      {#if settings.invited.length > 0}
        <h3>Invited from Syncplay</h3>
        <ul class="people">
          {#each settings.invited as i (i.shareId)}
            <li>
              <div class="spread">
                <strong class="selectable">{i.username}</strong>
                <button class="small" onclick={() => store.run(async () => (store.snap = await api.plexRemoveInvite(i.shareId)), `${i.username} removed`)}>
                  Remove
                </button>
              </div>
              <div class="muted selectable">Asked as {i.syncplayName} on {formatDate(i.at)}</div>
            </li>
          {/each}
        </ul>
      {/if}
    </section>

    <section class="card">
      <h2>Port</h2>
      <p class="sub">The port Plex listens on. Change it only if you changed it in Plex too.</p>
      <div class="fields">
        <label class="field">Plex port<input type="number" min="1024" max="65535" bind:value={port.draft.port} /></label>
      </div>
      <div class="row buttons">
        <button class="primary" disabled={!port.dirty} onclick={() => store.save((s) => Object.assign(s.plex, port.draft), "Port saved")}>Save</button>
        <button disabled={!port.dirty} onclick={() => port.reset()}>Revert</button>
      </div>
      <div class="upnp">
        <Toggle
          label="Forward the port with UPnP"
          hint={`Asks your router to forward TCP ${settings.port} to this PC. Leave Plex's own "Remote Access" port forwarding off when this is on.`}
          checked={settings.upnp}
          onchange={(v) => store.save((s) => (s.plex.upnp = v))}
        />
      </div>
    </section>
  {/if}
</div>

<style>
  h1 {
    font-size: 1.6em;
  }
  h3 {
    font-size: 1em;
    margin: 18px 0 8px;
  }
  .note {
    font-size: 0.9em;
    margin: 8px 0;
  }
  .addrs {
    margin-top: 8px;
  }
  .buttons {
    margin-top: 14px;
  }
  .upnp {
    margin-top: 14px;
    border-top: 1px solid var(--line);
    padding-top: 8px;
  }
  .setup {
    border-color: var(--accent);
  }
  .notice {
    margin-top: 10px;
  }
  .guide p {
    margin: 6px 0 10px;
  }
  .code {
    font-family: ui-monospace, monospace;
    font-size: 2em;
    font-weight: 700;
    letter-spacing: 0.2em;
    margin: 8px 0;
  }
  .checks {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 18px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .people {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .people li {
    background: var(--card-high);
    border-radius: 10px;
    padding: 10px 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
</style>
