<script lang="ts">
  import { api, errorText, type Library, type MediaCard, type MediaFile } from "./api";
  import { store } from "./store.svelte";
  import Art from "./Art.svelte";

  let { libraries }: { libraries: Library[] | null } = $props();

  const pageSize = 48;

  type Crumb = { id: string; name: string };
  type Pending = { item: MediaCard; file?: MediaFile };

  let stack = $state<Crumb[]>([]);
  let view = $state<"grid" | "list">("grid");
  let items = $state<MediaCard[]>([]);
  let total = $state(0);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let selected = $state<MediaCard | null>(null);
  let pending = $state<Pending | null>(null);
  let deleting = $state(false);
  let dialogEl = $state<HTMLDialogElement | null>(null);
  let autoOpened = $state(false);

  const folder = $derived(stack.at(-1) ?? null);
  const browsable = $derived(
    (libraries ?? []).filter((library) => library.itemId) as (Library & { itemId: string })[],
  );

  $effect(() => {
    if (autoOpened || stack.length > 0 || browsable.length !== 1) return;
    autoOpened = true;
    stack = [{ id: browsable[0].itemId, name: browsable[0].name }];
  });

  $effect(() => {
    const id = folder?.id;
    if (!id) return;
    load(id, true);
  });

  $effect(() => {
    if (!pending || !dialogEl) return;
    if (!dialogEl.open) dialogEl.showModal();
    dialogEl.querySelector<HTMLButtonElement>("[data-cancel]")?.focus();
  });

  async function load(parentId: string, reset: boolean) {
    const from = reset ? 0 : items.length;
    loading = true;
    if (reset) error = null;
    try {
      const page = await api.jellyfinBrowse(parentId, from, pageSize);
      if (folder?.id !== parentId) return;
      items = reset ? page.items : [...items, ...page.items];
      total = page.total;
      if (selected) selected = items.find((item) => item.id === selected!.id) ?? null;
    } catch (e) {
      if (folder?.id === parentId) {
        error = errorText(e);
        if (reset) items = [];
      }
    }
    if (folder?.id === parentId) loading = false;
  }

  function openLibrary(library: Library & { itemId: string }) {
    selected = null;
    stack = [{ id: library.itemId, name: library.name }];
  }

  function openItem(item: MediaCard) {
    if (item.canDrill) {
      selected = null;
      stack = [...stack, { id: item.id, name: item.name }];
      return;
    }
    selected = item;
  }

  function goTo(index: number) {
    selected = null;
    stack = stack.slice(0, index + 1);
  }

  function backToLibraries() {
    selected = null;
    stack = [];
  }

  function noun(kind: string): string {
    switch (kind) {
      case "Series":
        return "show";
      case "Season":
        return "season";
      case "Episode":
        return "episode";
      case "Movie":
        return "movie";
      case "Video":
        return "video";
      default:
        return "item";
    }
  }

  function episodeCode(item: MediaCard): string {
    if (item.parentIndexNumber != null && item.indexNumber != null) {
      return `S${String(item.parentIndexNumber).padStart(2, "0")}E${String(item.indexNumber).padStart(2, "0")}`;
    }
    if (item.indexNumber != null) return `Episode ${item.indexNumber}`;
    return item.name;
  }

  /** The dialog names exactly what Jellyfin will remove. */
  function deletePrompt(item: MediaCard, file?: MediaFile): string {
    if (file) {
      const also = file.removesItem
        ? ` It is this ${noun(item.kind)}'s only file, so the ${noun(item.kind)} is removed too.`
        : "";
      return `Delete the file “${file.name}”? Jellyfin deletes that file.${also}`;
    }
    switch (item.kind) {
      case "Series":
        return `Delete the show “${item.name}”? Jellyfin removes the show, its seasons and episodes, and their files.`;
      case "Season":
        return `Delete “${item.name}”${item.seriesName ? ` of “${item.seriesName}”` : ""}? Jellyfin removes that season, its episodes, and their files.`;
      case "Episode":
        return `Delete ${episodeCode(item)} “${item.name}”? Jellyfin removes that episode and its files.`;
      case "Movie":
        return `Delete the movie “${item.name}”? Jellyfin removes the movie and its files.`;
      default:
        return `Delete “${item.name}”? Jellyfin removes it and its files.`;
    }
  }

  function meta(item: MediaCard): string {
    if (item.kind === "Episode") {
      return [episodeCode(item), item.seriesName].filter(Boolean).join(" · ");
    }
    if (item.kind === "Season") return item.seriesName ?? "";
    const bits: string[] = [];
    if (item.year) bits.push(String(item.year));
    if (item.kind === "Series" && item.childCount != null) {
      bits.push(`${item.childCount} season${item.childCount === 1 ? "" : "s"}`);
    }
    return bits.join(" · ");
  }

  async function commitDelete() {
    if (!pending || deleting) return;
    deleting = true;
    const target = pending;
    const id = target.file?.id ?? target.item.id;
    const deleted = await store.run(async () => {
      await api.jellyfinDeleteItem(id, true);
      return true;
    }, "Deleted");
    deleting = false;
    if (!deleted) return;
    pending = null;
    if (!target.file || target.file.removesItem) selected = null;
    if (folder) await load(folder.id, true);
  }
</script>

<section class="card browser">
  <div class="spread">
    <div>
      <h2>Library</h2>
      <p class="sub">Shows and movies, with their Jellyfin artwork. Deleting asks you to confirm, then Jellyfin removes the files.</p>
    </div>
    {#if folder}
      <div class="row">
        <button class="small" class:primary={view === "grid"} aria-pressed={view === "grid"} onclick={() => (view = "grid")}>Grid</button>
        <button class="small" class:primary={view === "list"} aria-pressed={view === "list"} onclick={() => (view = "list")}>List</button>
      </div>
    {/if}
  </div>

  {#if libraries === null}
    <p class="muted">Loading…</p>
  {:else if !folder}
    {#if browsable.length === 0}
      <p class="muted">Add a library above, then open it here.</p>
    {:else}
      <ul class="libs">
        {#each browsable as library (library.itemId)}
          <li>
            <button class="texty" onclick={() => openLibrary(library)}>{library.name}</button>
          </li>
        {/each}
      </ul>
    {/if}
  {:else}
    <nav class="crumbs" aria-label="Library path">
      <button class="small" onclick={backToLibraries}>Libraries</button>
      {#each stack as crumb, index (crumb.id)}
        <span class="muted">/</span>
        {#if index === stack.length - 1}
          <span>{crumb.name}</span>
        {:else}
          <button class="small" onclick={() => goTo(index)}>{crumb.name}</button>
        {/if}
      {/each}
    </nav>

    {#if error}<div class="notice err">{error}</div>{/if}

    {#if items.length === 0 && !loading}
      <p class="muted">Nothing here yet. Refresh libraries if files were just added.</p>
    {:else}
      <div class={view} role="list">
        {#each items as item (item.id)}
          <article class="tile" class:open={selected?.id === item.id} role="listitem">
            <button class="open-item" onclick={() => openItem(item)}>
              <div class="poster">
                <Art itemId={item.id} image={item.poster} alt={item.name} />
                {#if view === "grid" && item.logo}
                  <div class="logo-slot">
                    <Art itemId={item.id} image={item.logo} alt="" />
                  </div>
                {/if}
              </div>
              <div class="title">
                <strong>{item.name}</strong>
                {#if meta(item)}<span class="muted">{meta(item)}</span>{/if}
              </div>
            </button>
            <button class="small danger" onclick={() => (pending = { item })}>Delete</button>
          </article>
        {/each}
      </div>
      {#if items.length < total}
        <div class="row buttons">
          <button class="small" disabled={loading} onclick={() => folder && load(folder.id, false)}>
            {loading ? "Loading…" : `Show more (${items.length} of ${total})`}
          </button>
        </div>
      {:else if loading}
        <p class="muted">Loading…</p>
      {/if}
    {/if}

    {#if selected}
      <div class="files">
        <div class="spread">
          <h3>{selected.name}</h3>
          <button class="small" onclick={() => (selected = null)}>Close</button>
        </div>
        {#if selected.files.length === 0}
          <p class="muted">Jellyfin has no video file listed for this {noun(selected.kind)}.</p>
        {:else}
          <ul class="file-list">
            {#each selected.files as file (file.id + file.path)}
              <li class="spread">
                <div>
                  <strong>{file.name}</strong>
                  <div class="muted path">{file.path}</div>
                </div>
                <button class="small danger" onclick={() => selected && (pending = { item: selected, file })}>Delete file</button>
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    {/if}
  {/if}
</section>

{#if pending}
  <dialog
    bind:this={dialogEl}
    class="dialog"
    aria-labelledby="delete-title"
    oncancel={(event) => {
      event.preventDefault();
      if (!deleting) pending = null;
    }}
  >
    <h2 id="delete-title">Delete this {pending.file ? "file" : noun(pending.item.kind)}?</h2>
    <p>{deletePrompt(pending.item, pending.file)}</p>
    <div class="row buttons">
      <button class="danger" disabled={deleting} onclick={commitDelete}>{deleting ? "Deleting…" : "Delete"}</button>
      <button data-cancel disabled={deleting} onclick={() => (pending = null)}>Cancel</button>
    </div>
  </dialog>
{/if}

<style>
  .browser .sub {
    max-width: 62ch;
  }
  .crumbs {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin-bottom: 12px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(148px, 1fr));
    gap: 14px;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .tile {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
  }
  .list .tile {
    flex-direction: row;
    align-items: center;
    gap: 12px;
  }
  .open-item {
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: stretch;
    text-align: left;
    background: transparent;
    border: 0;
    padding: 0;
    color: inherit;
    font-weight: 500;
    min-width: 0;
    white-space: normal;
  }
  .list .open-item {
    flex-direction: row;
    align-items: center;
    flex: 1;
  }
  .poster {
    position: relative;
    aspect-ratio: 2 / 3;
    border-radius: 12px;
    overflow: hidden;
    background: var(--bg);
    border: 1px solid var(--line);
  }
  .list .poster {
    width: 52px;
    flex: none;
  }
  .logo-slot {
    position: absolute;
    left: 10%;
    right: 10%;
    bottom: 8%;
    height: 28%;
    pointer-events: none;
  }
  .logo-slot :global(.art) {
    background: transparent;
  }
  .logo-slot :global(img) {
    object-fit: contain;
    filter: drop-shadow(0 1px 2px #000);
  }
  .logo-slot :global(.ph) {
    display: none;
  }
  .title {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .title strong,
  .title span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tile.open .poster {
    outline: 2px solid var(--accent);
  }
  .files {
    margin-top: 16px;
    padding-top: 12px;
    border-top: 1px solid var(--line);
  }
  .file-list {
    list-style: none;
    padding: 0;
    margin: 8px 0 0;
  }
  .file-list li + li {
    margin-top: 10px;
  }
  .path {
    font-size: 0.86em;
    word-break: break-all;
    user-select: text;
  }
  button.texty {
    background: transparent;
    border: 0;
    padding: 4px 0;
    color: var(--accent);
    font-weight: 600;
  }
  .libs {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  dialog.dialog {
    width: min(440px, calc(100% - 32px));
    border: 1px solid var(--line);
    border-radius: 16px;
    background: var(--card);
    color: var(--text);
    padding: 18px 20px;
  }
  dialog.dialog::backdrop {
    background: rgba(0, 0, 0, 0.55);
  }
  dialog.dialog p {
    margin: 10px 0 16px;
  }
</style>
