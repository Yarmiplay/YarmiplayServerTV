<script lang="ts" module>
  /** A media library, whichever server it belongs to. */
  export interface LibraryItem {
    id: string;
    name: string;
    kind: string;
    locations: string[];
  }
</script>

<script lang="ts">
  import { untrack } from "svelte";
  import { api } from "./api";

  let {
    libraries,
    kinds,
    kindLabel,
    folderNote = "",
    onAdd,
    onAddPath,
    onRemovePath,
    onRemove,
    onSort,
    canSort = () => true,
  }: {
    libraries: LibraryItem[] | null;
    /** [value, label] for the "Content" choice when adding a library. */
    kinds: [string, string][];
    kindLabel: (kind: string) => string;
    folderNote?: string;
    /** Resolves true when the library was added. */
    onAdd: (name: string, kind: string, path: string) => Promise<boolean>;
    onAddPath: (id: string, path: string) => void;
    onRemovePath: (id: string, path: string) => void;
    onRemove: (id: string, name: string) => void;
    /** Offers "Sort into shows" on each folder when set. */
    onSort?: (id: string, path: string) => void;
    canSort?: (id: string, path: string) => boolean;
  } = $props();

  let newName = $state("");
  // The kinds never change for a page; start on the first one.
  let newKind = $state(untrack(() => kinds[0][0]));
  let newPath = $state("");
  let confirmRemove = $state<string | null>(null);

  async function add() {
    if (await onAdd(newName, newKind, newPath)) {
      newName = "";
      newPath = "";
    }
  }

  async function addPath(id: string) {
    const dir = await api.pickFolder();
    if (dir) onAddPath(id, dir);
  }
</script>

{#if libraries === null}
  <p class="muted">Loading…</p>
{:else if libraries.length === 0}
  <p class="muted">No libraries yet. Add a folder with your movies or shows below.</p>
{:else}
  <ul class="libs">
    {#each libraries as lib (lib.id)}
      <li>
        <div class="spread">
          <div><strong>{lib.name}</strong> <span class="pill">{kindLabel(lib.kind)}</span></div>
          <div class="row">
            <button class="small" onclick={() => addPath(lib.id)}>Add folder</button>
            {#if confirmRemove === lib.id}
              <button
                class="small danger"
                onclick={() => {
                  confirmRemove = null;
                  onRemove(lib.id, lib.name);
                }}>Really remove?</button
              >
              <button class="small" onclick={() => (confirmRemove = null)}>Cancel</button>
            {:else}
              <button class="small danger" onclick={() => (confirmRemove = lib.id)}>Remove</button>
            {/if}
          </div>
        </div>
        {#each lib.locations as loc (loc)}
          <div class="loc spread">
            <code>{loc}</code>
            <span class="row">
            {#if onSort && canSort(lib.id, loc)}
              <button class="small" onclick={() => onSort(lib.id, loc)}>Sort into shows</button>
            {/if}
            <button
              class="small"
              onclick={() => onRemovePath(lib.id, loc)}
              disabled={lib.locations.length < 2}
              title={lib.locations.length < 2 ? "A library needs at least one folder" : ""}>Remove folder</button
            >
            </span>
          </div>
        {/each}
      </li>
    {/each}
  </ul>
{/if}

<h3>Add a library</h3>
<div class="fields">
  <label class="field">Name<input bind:value={newName} placeholder="Movies" /></label>
  <label class="field">
    Content
    <select bind:value={newKind}>
      {#each kinds as [k, label] (k)}<option value={k}>{label}</option>{/each}
    </select>
  </label>
  <label class="field">
    Folder
    <span class="row">
      <input bind:value={newPath} placeholder="Choose a folder" style="flex:1" />
      <button class="small" onclick={async () => (newPath = (await api.pickFolder()) ?? newPath)}>Browse</button>
    </span>
  </label>
</div>
{#if folderNote}<p class="muted note">{folderNote}</p>{/if}
<div class="row buttons">
  <button class="primary" disabled={!newName.trim() || !newPath.trim()} onclick={add}>Add library</button>
</div>

<style>
  h3 {
    font-size: 1em;
    margin: 20px 0 10px;
  }
  .note {
    font-size: 0.9em;
    margin: 8px 0;
  }
  .buttons {
    margin-top: 14px;
  }
  .libs {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .libs li {
    background: var(--card-high);
    border-radius: 10px;
    padding: 10px 14px;
  }
  .libs li button {
    background: var(--card);
  }
  .loc {
    margin-top: 6px;
  }
</style>
