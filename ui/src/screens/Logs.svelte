<script lang="ts">
  import { tick } from "svelte";
  import { api, copyText, type LogLine } from "../lib/api";
  import { store } from "../lib/store.svelte";

  const ranks: Record<LogLine["level"], number> = { error: 0, warn: 1, info: 2, debug: 3, trace: 4 };
  let level = $state<LogLine["level"]>("info");
  let filter = $state("");
  let follow = $state(true);
  let box: HTMLDivElement | undefined = $state();

  const lines = $derived.by(() => {
    const f = filter.trim().toLowerCase();
    return store.logs.filter(
      (l) => ranks[l.level] <= ranks[level] && (!f || l.message.toLowerCase().includes(f) || l.target.toLowerCase().includes(f)),
    );
  });

  $effect(() => {
    lines.length;
    if (follow && box) tick().then(() => box && (box.scrollTop = box.scrollHeight));
  });

  const time = (ms: number) => new Date(ms).toLocaleTimeString(undefined, { hour12: false });
  const text = () => lines.map((l) => `${new Date(l.time).toISOString()} ${l.level.toUpperCase()} ${l.target}: ${l.message}`).join("\n");

  async function clear() {
    await api.clearLogs();
    store.logs = [];
  }
</script>

<div class="logs">
  <header class="spread">
    <h1>Logs</h1>
    <div class="row">
      <select bind:value={level} aria-label="Level">
        <option value="error">Errors</option>
        <option value="warn">Warnings</option>
        <option value="info">Info</option>
        <option value="debug">Debug</option>
      </select>
      <input placeholder="Filter" bind:value={filter} />
      <label class="row follow"><input type="checkbox" bind:checked={follow} /> Follow</label>
      <button class="small" onclick={() => store.run(() => copyText(text()), "Copied")}>Copy</button>
      <button class="small" onclick={clear}>Clear</button>
    </div>
  </header>
  <div class="box mono selectable" bind:this={box}>
    {#each lines as l (l.seq)}
      <div class="line {l.level}"><span class="t">{time(l.time)}</span> <span class="lv">{l.level}</span> <span class="tg">{l.target}</span> {l.message}</div>
    {:else}
      <p class="muted">No log lines.</p>
    {/each}
  </div>
</div>

<style>
  .logs {
    display: flex;
    flex-direction: column;
    gap: 14px;
    height: calc(100vh - 66px);
  }
  h1 {
    font-size: 1.6em;
  }
  .follow {
    gap: 4px;
    color: var(--muted);
    font-size: 0.9em;
  }
  .box {
    flex: 1;
    overflow: auto;
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: 16px;
    padding: 12px 14px;
    font-size: 0.82em;
    line-height: 1.55;
  }
  .line {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .t,
  .tg {
    color: var(--muted);
  }
  .lv {
    display: inline-block;
    width: 3.6em;
    text-transform: uppercase;
    font-weight: 600;
    color: var(--muted);
  }
  .warn .lv {
    color: var(--warn);
  }
  .error .lv,
  .error {
    color: var(--err);
  }
  .info .lv {
    color: var(--accent);
  }
</style>
