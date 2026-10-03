<script lang="ts">
  import { api, copyText } from "./api";
  import { store } from "./store.svelte";

  let { label, value, link = false }: { label: string; value: string | null; link?: boolean } = $props();

  async function copy() {
    if (!value) return;
    await store.run(() => copyText(value!), "Copied");
  }
</script>

<div class="addr">
  <span class="label">{label}</span>
  {#if value}
    <div class="line">
      <code class="value" title={value}>{value}</code>
      <span class="actions">
        <button class="small" onclick={copy}>Copy</button>
        {#if link}<button class="small" onclick={() => api.openUrl(value!)}>Open</button>{/if}
      </span>
    </div>
  {:else}
    <span class="muted">–</span>
  {/if}
</div>

<style>
  .addr {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 5px 0;
  }
  .label {
    color: var(--muted);
    font-size: 0.85em;
  }
  .line {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .value {
    flex: 0 1 auto;
    min-width: 0;
    padding: 0.3em 0.6em;
    font-size: 0.85em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .actions {
    display: flex;
    gap: 6px;
    margin-left: auto;
  }
</style>
