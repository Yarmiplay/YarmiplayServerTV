<script lang="ts">
  import type { Library, Snapshot } from "../lib/api";
  import { api, errorText, formatBytes } from "../lib/api";
  import { store } from "../lib/store.svelte";
  import { useDraft } from "../lib/draft.svelte";
  import { jellyfinPill } from "../lib/status";
  import Toggle from "../lib/Toggle.svelte";
  import Address from "../lib/Address.svelte";
  import Libraries from "../lib/Libraries.svelte";

  let { snap }: { snap: Snapshot } = $props();

  const j = $derived(snap.jellyfin);
  const pill = $derived(jellyfinPill(snap));
  const running = $derived(j.phase === "running");
  const needsSetup = $derived(running && j.wizardCompleted === false);
  const needsLogin = $derived(running && j.wizardCompleted === true && !snap.jellyfinSignedIn);
  const signedIn = $derived(running && snap.jellyfinSignedIn);

  const ports = useDraft(() => ({ httpPort: snap.settings.jellyfin.httpPort, httpsPort: snap.settings.jellyfin.httpsPort }));

  // First-run setup
  let setupName = $state("YarmiplayServerTV");
  let setupUser = $state("");
  let setupPass = $state("");
  let setupPass2 = $state("");
  let busy = $state(false);
  let formError = $state<string | null>(null);

  async function runSetup() {
    formError = null;
    if (setupPass !== setupPass2) {
      formError = "The passwords don't match";
      return;
    }
    busy = true;
    try {
      store.snap = await api.jellyfinSetup(setupName, setupUser, setupPass);
      setupPass = setupPass2 = "";
      store.notify("Jellyfin is set up");
      await loadLibraries();
    } catch (e) {
      formError = errorText(e);
    }
    busy = false;
  }

  // Sign in
  let loginUser = $state("");
  let loginPass = $state("");
  async function login() {
    formError = null;
    busy = true;
    try {
      store.snap = await api.jellyfinLogin(loginUser, loginPass);
      loginPass = "";
      await loadLibraries();
    } catch (e) {
      formError = errorText(e);
    }
    busy = false;
  }

  // Libraries
  let libraries = $state<Library[] | null>(null);
  let libError = $state<string | null>(null);

  async function loadLibraries() {
    libError = null;
    try {
      libraries = await api.jellyfinLibraries();
    } catch (e) {
      libError = errorText(e);
    }
  }

  $effect(() => {
    if (signedIn && libraries === null) loadLibraries();
  });

  async function addLibrary(name: string, type: string, path: string): Promise<boolean> {
    const ok = await store.run(() => api.jellyfinAddLibrary(name, type, path), `Added ${name}`);
    await loadLibraries();
    return ok !== undefined;
  }

  async function addPath(lib: string, dir: string) {
    await store.run(() => api.jellyfinAddPath(lib, dir), "Folder added");
    await loadLibraries();
  }

  async function removePath(lib: string, path: string) {
    await store.run(() => api.jellyfinRemovePath(lib, path), "Folder removed");
    await loadLibraries();
  }

  async function removeLibrary(name: string) {
    await store.run(() => api.jellyfinRemoveLibrary(name), `Removed ${name}`);
    await loadLibraries();
  }

  const types: [string, string][] = [
    ["movies", "Movies"],
    ["tvshows", "Shows"],
    ["music", "Music"],
    ["homevideos", "Home videos & photos"],
    ["mixed", "Mixed movies & shows"],
  ];
  const typeLabel = (t: string) => types.find(([k]) => k === t)?.[1] ?? "Mixed";
</script>

<div class="stack">
  <header class="spread">
    <h1>Jellyfin</h1>
    <span class="pill {pill.kind}">{pill.text}</span>
  </header>

  <section class="card">
    <Toggle
      label="Jellyfin server"
      hint={j.installedVersion
        ? `Jellyfin ${j.installedVersion}`
        : `Switching on downloads Jellyfin ${j.pinnedVersion} (about 150–250 MB) from repo.jellyfin.org`}
      checked={snap.settings.jellyfin.enabled}
      onchange={(v) => store.save((s) => (s.jellyfin.enabled = v))}
    />
    {#if j.progress && (j.phase === "downloading" || j.phase === "installing")}
      <div class="progress" aria-label="Download progress">
        <div style="width:{j.progress.total ? (j.progress.downloaded / j.progress.total) * 100 : 0}%"></div>
      </div>
      <p class="muted note">
        {j.phase === "installing" ? "Unpacking…" : `${formatBytes(j.progress.downloaded)} of ${formatBytes(j.progress.total)}`}
      </p>
    {/if}
    {#if j.phase === "error" && j.error}
      <div class="notice err row spread">
        <span>{j.error}</span>
        <button class="small" onclick={() => api.jellyfinRetry()}>Retry now</button>
      </div>
    {/if}
    {#if snap.settings.jellyfin.enabled}
      <div class="addrs">
        <Address label="This PC" value={snap.addresses.jellyfinLocal} link />
        <Address label="On your network" value={snap.addresses.jellyfinLan} />
        <Address label="Over the internet" value={snap.addresses.jellyfinPublic} />
      </div>
      <p class="muted note">
        {#if j.https}
          HTTPS is on with the certificate for <code>{snap.tls.host}</code>.
        {:else if snap.settings.tls.enabled}
          HTTPS turns on once the DuckDNS certificate is ready.
        {:else}
          Set up DuckDNS on the Network page to serve Jellyfin over HTTPS.
        {/if}
      </p>
      <div class="row">
        <button class="small" onclick={() => api.openFolder("jellyfin-logs")}>Jellyfin log folder</button>
      </div>
    {/if}
  </section>

  {#if needsSetup}
    <section class="card setup">
      <h2>Set up Jellyfin</h2>
      <p class="sub">Create the administrator account. You use it to sign in to Jellyfin from YarmiplayTV.</p>
      <form
        onsubmit={(e) => {
          e.preventDefault();
          runSetup();
        }}
      >
        <div class="fields">
          <label class="field">Server name<input bind:value={setupName} /></label>
          <label class="field">Administrator username<input bind:value={setupUser} autocomplete="off" required /></label>
          <label class="field">Password<input type="password" bind:value={setupPass} autocomplete="new-password" required /></label>
          <label class="field">Repeat password<input type="password" bind:value={setupPass2} autocomplete="new-password" required /></label>
        </div>
        {#if formError}<div class="notice err">{formError}</div>{/if}
        <div class="row buttons"><button class="primary" type="submit" disabled={busy}>{busy ? "Setting up…" : "Finish setup"}</button></div>
      </form>
    </section>
  {:else if needsLogin}
    <section class="card">
      <h2>Sign in</h2>
      <p class="sub">Sign in with a Jellyfin administrator account to manage libraries from here.</p>
      <form
        onsubmit={(e) => {
          e.preventDefault();
          login();
        }}
      >
        <div class="fields">
          <label class="field">Username<input bind:value={loginUser} autocomplete="username" required /></label>
          <label class="field">Password<input type="password" bind:value={loginPass} autocomplete="current-password" /></label>
        </div>
        {#if formError}<div class="notice err">{formError}</div>{/if}
        <div class="row buttons"><button class="primary" type="submit" disabled={busy}>Sign in</button></div>
      </form>
    </section>
  {:else if signedIn}
    <section class="card">
      <div class="spread">
        <div>
          <h2>Libraries</h2>
          <p class="sub">
            Signed in as <strong>{snap.settings.jellyfin.adminUser}</strong>{j.serverName ? ` on ${j.serverName}` : ""}.
          </p>
        </div>
        <div class="row">
          <button class="small" onclick={() => store.run(() => api.jellyfinRescan(), "Scanning libraries")}>Scan all</button>
          <button class="small" onclick={async () => (store.snap = await api.jellyfinLogout())}>Sign out</button>
        </div>
      </div>
      {#if libError}<div class="notice err">{libError}</div>{/if}
      <Libraries
        libraries={libraries?.map((l) => ({ id: l.name, name: l.name, kind: l.collectionType ?? "mixed", locations: l.locations })) ?? null}
        kinds={types}
        kindLabel={typeLabel}
        onAdd={addLibrary}
        onAddPath={addPath}
        onRemovePath={removePath}
        onRemove={(name) => removeLibrary(name)}
      />
    </section>
  {/if}

  <section class="card">
    <Toggle
      label="Share with Syncplay users"
      hint={snap.settings.syncplay.vanillaMode
        ? "Off in vanilla Syncplay mode"
        : "People on your Syncplay server can add this Jellyfin in YarmiplayTV with one tap, signed in as a hidden guest account that can watch but not change anything"}
      disabled={snap.settings.syncplay.vanillaMode}
      checked={snap.settings.jellyfin.shareWithSyncplay && !snap.settings.syncplay.vanillaMode}
      onchange={(v) => store.save((s) => (s.jellyfin.shareWithSyncplay = v))}
    />
    {#if snap.settings.jellyfin.shareWithSyncplay && !snap.settings.syncplay.vanillaMode}
      {#if snap.settings.syncplay.access === "open"}
        <div class="notice warn">
          Anyone can join the Syncplay server, so anyone who finds it can watch your libraries. On the Syncplay page,
          choose a password option or approved devices only under "Who can join".
        </div>
      {/if}
      {#if snap.jellyfinShare.error}<div class="notice err">{snap.jellyfinShare.error}</div>{/if}
      <p class="muted note">
        {#if snap.jellyfinShare.active}
          Shared. Turning this off signs every guest out.
        {:else if !snap.settings.syncplay.enabled}
          Turn on the Syncplay server to share Jellyfin with its users.
        {:else if !signedIn}
          Sharing starts once Jellyfin is running and you're signed in here.
        {:else}
          Setting up the guest account…
        {/if}
      </p>
    {/if}
  </section>

  <section class="card">
    <h2>Ports</h2>
    <p class="sub">Changing a port restarts Jellyfin.</p>
    <div class="fields">
      <label class="field">HTTP port<input type="number" min="1024" max="65535" bind:value={ports.draft.httpPort} /></label>
      <label class="field">HTTPS port<input type="number" min="1024" max="65535" bind:value={ports.draft.httpsPort} /></label>
    </div>
    <div class="row buttons">
      <button class="primary" disabled={!ports.dirty} onclick={() => store.save((s) => Object.assign(s.jellyfin, ports.draft), "Ports saved")}>Save</button>
      <button disabled={!ports.dirty} onclick={() => ports.reset()}>Revert</button>
    </div>
    <div class="upnp">
      <Toggle
        label="Forward the ports with UPnP"
        hint={`Asks your router to forward TCP ${snap.settings.jellyfin.httpPort}${j.https ? ` and ${snap.settings.jellyfin.httpsPort}` : ""} to this PC`}
        checked={snap.settings.jellyfin.upnp}
        onchange={(v) => store.save((s) => (s.jellyfin.upnp = v))}
      />
    </div>
  </section>
</div>

<style>
  h1 {
    font-size: 1.6em;
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
  .progress {
    height: 8px;
    border-radius: 99px;
    background: var(--card-high);
    overflow: hidden;
    margin-top: 10px;
  }
  .progress div {
    height: 100%;
    background: var(--accent);
    transition: width 0.3s;
  }
  .notice {
    margin-top: 10px;
  }
</style>
