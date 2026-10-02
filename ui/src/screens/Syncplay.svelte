<script lang="ts">
  import type { Snapshot } from "../lib/api";
  import { store } from "../lib/store.svelte";
  import { useDraft } from "../lib/draft.svelte";
  import { syncplayPill } from "../lib/status";
  import Toggle from "../lib/Toggle.svelte";
  import Address from "../lib/Address.svelte";

  let { snap }: { snap: Snapshot } = $props();

  const form = useDraft(() => {
    const { enabled, upnp, ...rest } = snap.settings.syncplay;
    return rest;
  });
  let showPassword = $state(false);
  let saving = $state(false);

  async function save() {
    saving = true;
    await store.save((s) => Object.assign(s.syncplay, form.draft), "Syncplay settings saved");
    saving = false;
  }

  const pill = $derived(syncplayPill(snap));
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
      <p class="muted small">
        {#if snap.syncplay.tls}
          Encrypted connections are available with the certificate for <code>{snap.tls.host}</code>. Connect with that
          name so clients can verify it.
        {:else}
          Connections are unencrypted. Set up DuckDNS on the Network page to enable TLS.
        {/if}
        {#if snap.settings.syncplay.password}Clients need the server password.{/if}
      </p>
    {/if}
  </section>

  <section class="card">
    <h2>Settings</h2>
    <p class="sub">Changing the port restarts the server and disconnects everyone.</p>
    <div class="fields">
      <label class="field">Port<input type="number" min="1024" max="65535" bind:value={form.draft.port} /></label>
      <label class="field">
        Server password (optional)
        <span class="row">
          <input
            type={showPassword ? "text" : "password"}
            bind:value={form.draft.password}
            placeholder="No password"
            autocomplete="off"
            style="flex:1"
          />
          <button class="small" onclick={() => (showPassword = !showPassword)}>{showPassword ? "Hide" : "Show"}</button>
        </span>
      </label>
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
  .small {
    font-size: 0.9em;
    margin: 10px 0 0;
  }
  .motd {
    margin-top: 14px;
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
</style>
