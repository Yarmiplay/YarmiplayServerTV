<script lang="ts">
  import { api, inApp, type ImageRef } from "./api";

  let {
    itemId,
    image,
    alt,
  }: {
    itemId: string;
    image: ImageRef | null;
    alt: string;
  } = $props();

  let host: HTMLElement | undefined = $state();
  let src = $state<string | null>(null);
  let failed = $state(false);
  let loaded = $state(false);

  function browserSrc(id: string, picture: ImageRef): string {
    const tag = /^[A-Za-z0-9]+$/.test(picture.tag) ? `?tag=${picture.tag}` : "";
    return `/api/jellyfin-image/${encodeURIComponent(id)}/${encodeURIComponent(picture.kind)}${tag}`;
  }

  $effect(() => {
    const id = itemId;
    const picture = image;
    src = null;
    failed = false;
    loaded = false;
    if (!picture) return;

    if (!inApp) {
      src = browserSrc(id, picture);
      return;
    }

    const el = host;
    if (!el) return;
    let gone = false;
    let objectUrl: string | null = null;
    const io = new IntersectionObserver(
      (entries) => {
        if (!entries.some((entry) => entry.isIntersecting)) return;
        io.disconnect();
        api
          .jellyfinArtwork(id, picture.kind)
          .then((artwork) => {
            if (gone) return;
            objectUrl = URL.createObjectURL(new Blob([new Uint8Array(artwork.data)], { type: artwork.contentType }));
            src = objectUrl;
          })
          .catch(() => {
            if (!gone) failed = true;
          });
      },
      { rootMargin: "240px" },
    );
    io.observe(el);
    return () => {
      gone = true;
      io.disconnect();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  });
</script>

<div class="art" bind:this={host}>
  {#if src && !failed}
    <img
      {src}
      {alt}
      loading="lazy"
      class:ready={loaded}
      onload={() => (loaded = true)}
      onerror={() => (failed = true)}
    />
  {/if}
  {#if !src || failed || !loaded}
    <span class="ph" aria-hidden="true">{alt.slice(0, 1).toUpperCase() || "?"}</span>
  {/if}
</div>

<style>
  .art {
    position: relative;
    width: 100%;
    height: 100%;
    background: var(--bg);
  }
  img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    opacity: 0;
  }
  img.ready {
    opacity: 1;
  }
  .ph {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    color: var(--muted);
    font-size: 1.8em;
    font-weight: 700;
  }
</style>
