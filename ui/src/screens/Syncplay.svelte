<script lang="ts">
  import type { Snapshot } from "../lib/api";
  import { api, formatBytes, formatDateTime } from "../lib/api";
  import { store } from "../lib/store.svelte";
  import { useDraft } from "../lib/draft.svelte";
  import { syncplayPill } from "../lib/status";
  import Toggle from "../lib/Toggle.svelte";
  import Address from "../lib/Address.svelte";

  let { snap }: { snap: Snapshot } = $props();

  const form = useDraft(() => {
    const { enabled, upnp, vanillaMode, fileRelay, relayCacheGb, ...rest } = snap.settings.syncplay;
    return rest;
  });
  const cache = useDraft(() => ({ relayCacheGb: snap.settings.syncplay.relayCacheGb }));
  let showPassword = $state(false);
  let saving = $state(false);

  async function save() {
    saving = true;
    await store.save((s) => Object.assign(s.syncplay, form.draft), "Syncplay settings saved");
    saving = false;
  }

  async function clearCache() {
    const next = await store.run(() => api.clearRelayCache(), "Relay cache cleared");
    if (next) store.snap = next;
  }

  async function device(call: () => Promise<Snapshot>, done?: string) {
    const next = await store.run(call, done);
    if (next) store.snap = next;
  }

  let renaming = $state<string | null>(null);
  let newName = $state("");
  function startRename(fingerprint: string, name: string) {
    renaming = fingerprint;
    newName = name;
  }
  async function rename() {
    if (!renaming) return;
    const fp = renaming;
    renaming = null;
    await device(() => api.renameDevice(fp, newName));
  }

  const pill = $derived(syncplayPill(snap));
  const vanilla = $derived(snap.settings.syncplay.vanillaMode);
  const access = $derived(snap.settings.syncplay.access);
  const relay = $derived(snap.syncplay.relay);
  const devices = $derived(snap.syncplay.devices);
  const OFF_IN_VANILLA = "Off in vanilla Syncplay mode";

  function rate(bytesPerSecond: number): string {
    return bytesPerSecond > 0 ? `${((bytesPerSecond * 8) / 1e6).toFixed(1)} Mbit/s` : "idle";
  }
</script>

<div class="stack">
  <header class="spread">
    <h1>Syncplay</h1>
    <span class="pill {pill.kind}">{pill.text}</span>
  </header>

  <section class="card">
    <Toggle
      label="Syncplay server"
      hint="A Syncplay 1.7-compatible server built into this app"
      checked={snap.settings.syncplay.enabled}
      onchange={(v) => store.save((s) => (s.syncplay.enabled = v))}
    />
    {#if snap.syncplay.error}<div class="notice err">{snap.syncplay.error}</div>{/if}
    {#if snap.settings.syncplay.enabled}
      <div class="addrs">
        <Address label="On your network" value={snap.addresses.syncplayLan} />
        <Address label="Over the internet" value={snap.addresses.syncplayPublic} />
      </div>
      <p class="muted note">
        {#if snap.syncplay.tls}
          Encrypted connections are available with the certificate for <code>{snap.tls.host}</code>. Connect with that
          name so clients can verify it.
        {:else}
          Connections are unencrypted. Set up DuckDNS on the Network page to enable TLS.
        {/if}
        {#if access === "password"}
          Clients need the server password{vanilla ? "." : ", except YarmiplayTV devices you approved."}
        {:else if access === "approved"}
          Only YarmiplayTV devices you approve can join; official Syncplay clients are turned away.
        {:else}
          Anyone who knows the address can join.
        {/if}
      </p>
    {/if}
  </section>

  <section class="card">
    <h2>Settings</h2>
    <p class="sub">Changing the port restarts the server and disconnects everyone.</p>
    <div class="fields">
      <label class="field">Port<input type="number" min="1024" max="65535" bind:value={form.draft.port} /></label>
      <label class="field">
        Who can join
        <select bind:value={form.draft.access}>
          <option value="open">Anyone</option>
          <option value="password">Password</option>
          <option value="approved" disabled={vanilla}>Approved devices only</option>
        </select>
      </label>
      {#if form.draft.access === "password"}
        <label class="field">
          Server password
          <span class="secret">
            <input
              type={showPassword ? "text" : "password"}
              bind:value={form.draft.password}
              placeholder="Required"
              autocomplete="off"
            />
            <button class="small" onclick={() => (showPassword = !showPassword)}>{showPassword ? "Hide" : "Show"}</button>
          </span>
        </label>
      {/if}
      <label class="field">Max chat message length<input type="number" min="1" bind:value={form.draft.maxChatMessageLength} /></label>
      <label class="field">Max username length<input type="number" min="1" bind:value={form.draft.maxUsernameLength} /></label>
    </div>
    <label class="field motd">
      Message of the day
      <textarea rows="3" bind:value={form.draft.motd} placeholder="Shown to everyone who joins"></textarea>
    </label>
    <div class="toggles">
      <Toggle
        label="Isolate rooms"
        hint="Users only see people in their own room"
        checked={form.draft.isolateRooms}
        onchange={(v) => (form.draft.isolateRooms = v)}
      />
      <Toggle label="Disable chat" checked={form.draft.disableChat} onchange={(v) => (form.draft.disableChat = v)} />
      <Toggle
        label="Disable readiness"
        hint="Hide the ready / not ready indicator"
        checked={form.draft.disableReady}
        onchange={(v) => (form.draft.disableReady = v)}
      />
    </div>
    <div class="row buttons">
      <button class="primary" disabled={!form.dirty || saving} onclick={save}>Save</button>
      <button disabled={!form.dirty || saving} onclick={() => form.reset()}>Revert</button>
    </div>
  </section>

  <section class="card">
    <h2>Devices</h2>
    <p class="sub">
      YarmiplayTV devices prove who they are with a key of their own. Compare the code with the one the device shows
      before approving it.
    </p>
    {#if vanilla}
      <div class="notice">The list isn't in use while vanilla Syncplay mode is on.</div>
    {:else if access === "open"}
      <div class="notice">Devices are only checked when joining needs a password or approval.</div>
    {/if}

    {#if devices.pending.length > 0}
      <h3>Waiting for approval</h3>
      <ul class="devices">
        {#each devices.pending as d (d.fingerprint)}
          <li>
            <div class="spread">
              <strong class="selectable">{d.name}</strong>
              <code class="selectable">{d.fingerprint}</code>
            </div>
            <div class="muted selectable">
              {d.username || "No username"} · {d.ip} · {formatDateTime(d.requestedAt)}{d.connected ? "" : " · gave up waiting"}
            </div>
            <div class="row">
              <button class="small primary" onclick={() => device(() => api.approveDevice(d.fingerprint), `${d.name} approved`)}>
                Approve
              </button>
              <button class="small" onclick={() => device(() => api.denyDevice(d.fingerprint))}>Deny</button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}

    <h3>Approved</h3>
    {#if devices.approved.length === 0}
      <p class="muted">No devices yet.</p>
    {:else}
      <ul class="devices">
        {#each devices.approved as d (d.fingerprint)}
          <li>
            <div class="spread">
              {#if renaming === d.fingerprint}
                <span class="row">
                  <input
                    bind:value={newName}
                    maxlength="60"
                    aria-label="Device name"
                    onkeydown={(e) => {
                      if (e.key === "Enter") rename();
                      else if (e.key === "Escape") renaming = null;
                    }}
                  />
                  <button class="small primary" onclick={rename}>Save</button>
                  <button class="small" onclick={() => (renaming = null)}>Cancel</button>
                </span>
              {:else}
                <strong class="selectable">{d.name}</strong>
              {/if}
              <code class="selectable">{d.fingerprint}</code>
            </div>
            <div class="muted selectable">
              Last seen {formatDateTime(d.lastSeen)}{d.lastUsername ? ` as ${d.lastUsername}` : ""}
            </div>
            {#if renaming !== d.fingerprint}
              <div class="row">
                <button class="small" onclick={() => startRename(d.fingerprint, d.name)}>Rename</button>
                <button class="small" onclick={() => device(() => api.removeDevice(d.fingerprint), `${d.name} removed`)}>
                  Remove
                </button>
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="card">
    <h2>YarmiplayTV extras</h2>
    <p class="sub">Extra features for YarmiplayTV clients. Official Syncplay clients never see them.</p>
    <Toggle
      label="Vanilla Syncplay mode"
      hint={access === "approved" && !vanilla
        ? "Choose another way to join first: approved devices need YarmiplayTV features"
        : "Behave exactly like the official Syncplay server. YarmiplayTV features such as approved devices, the file relay and Jellyfin sharing are off."}
      disabled={access === "approved" && !vanilla}
      checked={vanilla}
      onchange={(v) => store.save((s) => (s.syncplay.vanillaMode = v))}
    />
    <Toggle
      label="File relay"
      hint={vanilla
        ? OFF_IN_VANILLA
        : "People in a room can play each other's video files, streamed through this PC and cached on its disk for a day"}
      disabled={vanilla}
      checked={snap.settings.syncplay.fileRelay && !vanilla}
      onchange={(v) => store.save((s) => (s.syncplay.fileRelay = v))}
    />
    <div class="cache" class:off={vanilla || !snap.settings.syncplay.fileRelay}>
      <label class="field">
        Cache size (GB)
        <span class="row">
          <input type="number" min="1" max="2000" bind:value={cache.draft.relayCacheGb} disabled={vanilla} />
          <button
            class="small"
            disabled={!cache.dirty || vanilla}
            onclick={() => store.save((s) => Object.assign(s.syncplay, cache.draft), "Cache size saved")}>Save</button
          >
        </span>
      </label>
      <p class="muted note">
        {formatBytes(relay.cacheBytes)} of {formatBytes(relay.cacheLimitBytes)} used. The cache is emptied when the app
        starts, and at least 2 GB of the disk always stays free.
      </p>
      <div class="row">
        <button class="small" onclick={clearCache} disabled={relay.cacheBytes === 0}>Clear cache</button>
      </div>
    </div>
  </section>

  <section class="card">
    <Toggle
      label="Forward the port with UPnP"
      hint={`Asks your router to forward TCP ${snap.settings.syncplay.port} to this PC`}
      checked={snap.settings.syncplay.upnp}
      onchange={(v) => store.save((s) => (s.syncplay.upnp = v))}
    />
  </section>

  <section class="card">
    <h2>Rooms</h2>
    {#if !snap.syncplay.running}
      <p class="muted">The server is off.</p>
    {:else if snap.syncplay.rooms.length === 0}
      <p class="muted">Nobody is connected.</p>
    {:else}
      <ul class="rooms">
        {#each snap.syncplay.rooms as room (room.name)}
          <li>
            <div class="spread">
              <strong class="selectable">{room.name}</strong>
              <span class="pill">{room.paused ? "Paused" : "Playing"}</span>
            </div>
            <div class="muted selectable">{room.users.join(", ")}</div>
            {#each relay.active.filter((a) => a.room === room.name) as a (a.name)}
              <div class="relay muted">
                Relaying <strong class="selectable">{a.name}</strong>: {formatBytes(a.cachedBytes)} of {formatBytes(a.size)}
                cached, {a.sources}
                {a.sources === 1 ? "source" : "sources"}, {a.readers}
                {a.readers === 1 ? "viewer" : "viewers"}, {rate(a.rate)}
              </div>
            {/each}
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</div>

<style>
  h1 {
    font-size: 1.6em;
  }
  .addrs {
    margin-top: 8px;
  }
  .note {
    font-size: 0.9em;
    margin: 10px 0 0;
  }
  .fields {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
  @media (min-width: 1300px) {
    .fields {
      grid-template-columns: repeat(4, minmax(0, 1fr));
    }
  }
  .fields input {
    width: 100%;
    box-sizing: border-box;
  }
  .secret {
    position: relative;
    display: block;
  }
  .secret input {
    padding-right: 64px;
  }
  .secret button {
    position: absolute;
    top: 50%;
    right: 5px;
    transform: translateY(-50%);
  }
  .motd {
    margin-top: 14px;
  }
  .motd textarea {
    resize: vertical;
  }
  .toggles {
    margin-top: 10px;
  }
  .buttons {
    margin-top: 14px;
  }
  .rooms {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .rooms li {
    background: var(--card-high);
    border-radius: 10px;
    padding: 10px 14px;
  }
  .notice {
    margin-top: 8px;
  }
  .cache {
    margin-top: 10px;
    border-top: 1px solid var(--line);
    padding-top: 10px;
  }
  .cache.off {
    opacity: 0.6;
  }
  .cache input {
    width: 110px;
  }
  .relay {
    font-size: 0.9em;
    margin-top: 4px;
  }
  h3 {
    font-size: 1em;
    margin: 14px 0 8px;
  }
  .devices {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .devices li {
    background: var(--card-high);
    border-radius: 10px;
    padding: 10px 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
</style>
