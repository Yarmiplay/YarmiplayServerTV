<script lang="ts">
  import type { Snapshot } from "../lib/api";
  import { store } from "../lib/store.svelte";
  import { useDraft } from "../lib/draft.svelte";
  import { voicePill } from "../lib/status";
  import Toggle from "../lib/Toggle.svelte";

  let { snap }: { snap: Snapshot } = $props();

  const limits = useDraft(() => ({
    voiceMaxParticipants: snap.settings.syncplay.voiceMaxParticipants,
    voiceMaxBitrate: snap.settings.syncplay.voiceMaxBitrate,
    voiceMaxPacketBytes: snap.settings.syncplay.voiceMaxPacketBytes,
    voiceActivityPerSec: snap.settings.syncplay.voiceActivityPerSec,
  }));

  const pill = $derived(voicePill(snap));
  const vanilla = $derived(snap.settings.syncplay.vanillaMode);
  const port = $derived(snap.settings.syncplay.port);
  const live = $derived(snap.voice);
  const OFF_IN_VANILLA = "Off while vanilla Syncplay mode is on. Turn that off on the Syncplay page.";

  function saveLimits() {
    store.save((s) => Object.assign(s.syncplay, limits.draft), "Voice limits saved. They take effect immediately.");
  }
</script>

<div class="stack">
  <header class="spread">
    <h1>Voice</h1>
    <span class="pill {pill.kind}">{pill.text}</span>
  </header>
  <p class="muted">
    Room voice is relayed on Syncplay port {port}. There is no extra port to open. Every switch here takes effect
    immediately. No restart.
  </p>

  <section class="card">
    <h2>Status</h2>
    <ul class="status">
      <li>
        <span>Relay</span>
        <strong>
          {#if vanilla}
            Off, vanilla mode
          {:else if !live.settingOn}
            Disabled
          {:else if !snap.settings.syncplay.enabled}
            Waiting for Syncplay
          {:else if live.effective}
            Enabled
          {:else}
            Starting
          {/if}
        </strong>
      </li>
      <li>
        <span>Syncplay port</span>
        <strong>{port}</strong>
      </li>
      <li>
        <span>People in voice</span>
        <strong>{live.activeUsers}</strong>
      </li>
    </ul>
    {#if vanilla}
      <p class="notice warn">Vanilla Syncplay mode is on, so voice is off even if the switch below is saved on.</p>
    {/if}
    {#if live.rooms.length === 0}
      <p class="muted">No one is in voice.</p>
    {:else}
      <ul class="rooms">
        {#each live.rooms as room (room.room)}
          <li>
            <span class="selectable">{room.room}</span>
            <span class="pill">{room.users === 1 ? "1 person" : `${room.users} people`}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="card">
    <Toggle
      label="Voice chat"
      hint={vanilla
        ? OFF_IN_VANILLA
        : `Relayed on Syncplay port ${port}. No extra port to open. Takes effect immediately. When off, clients are told "voice disabled on this server".`}
      disabled={vanilla}
      checked={snap.settings.syncplay.voice && !vanilla}
      onchange={(v) =>
        store.save(
          (s) => (s.syncplay.voice = v),
          v
            ? `Voice chat is on. It uses Syncplay port ${port} and takes effect immediately.`
            : `Voice chat is off. Clients are told "voice disabled on this server". Takes effect immediately.`,
        )}
    />
    <Toggle
      label="In-band FEC"
      hint="Tell clients to send Opus in-band forward error correction. Advertised immediately. No restart."
      disabled={vanilla}
      checked={snap.settings.syncplay.voiceFec}
      onchange={(v) => store.save((s) => (s.syncplay.voiceFec = v), "FEC setting saved. Clients are told immediately.")}
    />
    <Toggle
      label="DTX"
      hint="Tell clients to send Opus discontinuous transmission (silence is not sent). Advertised immediately. No restart."
      disabled={vanilla}
      checked={snap.settings.syncplay.voiceDtx}
      onchange={(v) => store.save((s) => (s.syncplay.voiceDtx = v), "DTX setting saved. Clients are told immediately.")}
    />
    <Toggle
      label="Log dropped frames"
      hint="Write a log line when a voice frame is dropped. Delivery failures are always logged, even when this is off. Takes effect immediately. Frames, tokens, and keys are never logged."
      checked={snap.settings.syncplay.voiceDiagnostics}
      onchange={(v) =>
        store.save((s) => (s.syncplay.voiceDiagnostics = v), "Voice logging saved. Takes effect immediately.")}
    />
  </section>

  <section class="card">
    <h2>Limits</h2>
    <p class="sub">Saved limits apply to the running relay immediately. No restart.</p>
    <div class="fields">
      <label class="field">
        Seats per room
        <input type="number" min="1" max="32" bind:value={limits.draft.voiceMaxParticipants} />
        <span class="hint">1–32. A room that is already over the new cap keeps its current people; the next join is refused.</span>
      </label>
      <label class="field">
        Max bitrate (bit/s)
        <input type="number" min="6000" max="128000" step="1000" bind:value={limits.draft.voiceMaxBitrate} />
        <span class="hint">6000–128000. Clients are told the new Opus target immediately.</span>
      </label>
      <label class="field">
        Max packet (bytes)
        <input type="number" min="56" max="1200" bind:value={limits.draft.voiceMaxPacketBytes} />
        <span class="hint">56–1200. Larger frames are dropped immediately. 1200 is the protocol ceiling.</span>
      </label>
      <label class="field">
        Speaking updates / second
        <input type="number" min="1" max="30" bind:value={limits.draft.voiceActivityPerSec} />
        <span class="hint">1–30 per person. Extra speaking indicators are dropped immediately.</span>
      </label>
    </div>
    <div class="row buttons">
      <button class="primary" disabled={!limits.dirty} onclick={saveLimits}>Save limits</button>
      <button disabled={!limits.dirty} onclick={() => limits.reset()}>Revert</button>
    </div>
  </section>

  <section class="card">
    <h2>Fixed by the protocol</h2>
    <p class="sub">These are not settings. Version 1 of the voice protocol requires them.</p>
    <ul class="status">
      <li><span>Codec</span><strong>Opus</strong></li>
      <li><span>Clock</span><strong>48000 Hz</strong></li>
      <li><span>Channels</span><strong>1 (mono)</strong></li>
      <li><span>Frame</span><strong>20 ms</strong></li>
      <li><span>Transport</span><strong>yv1 on the Syncplay connection</strong></li>
    </ul>
  </section>
</div>

<style>
  .status,
  .rooms {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .status li,
  .rooms li {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: center;
  }
  .status span,
  .hint {
    color: var(--muted);
  }
  .hint {
    font-size: 0.85em;
  }
  .notice {
    margin-top: 10px;
  }
</style>
