<script lang="ts">
  import { writeText } from "@tauri-apps/plugin-clipboard-manager";
  import { api } from "./api";
  import { store } from "./store.svelte";

  let { label, value, link = false }: { label: string; value: string | null; link?: boolean } = $props();

  async function copy() {
    if (!value) return;
    await store.run(() => writeText(value!), "Copied");
  }
</script>

<div class="addr">
  <span class="label">{label}</span>
  {#if value}
    <code class="value">{value}</code>
    <span class="actions">
      <button class="small" onclick={copy}>Copy</button>
      {#if link}<button class="small" onclick={() => api.openUrl(value!)}>Open</button>{/if}
    </span>
  {:else}
    <span class="muted value">–</span>
  {/if}
</div>

<style>
  .addr {
    display: grid;
    grid-template-columns: 120px 1fr auto;
    align-items: center;
    gap: 10px;
    padding: 4px 0;
  }
  .label {
    color: var(--muted);
    font-size: 0.9em;
  }
  .value {
    overflow-wrap: anywhere;
    justify-self: start;
  }
  .actions {
    display: flex;
    gap: 6px;
  }
</style>
