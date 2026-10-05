<script lang="ts">
  import logo from "../assets/logo.svg";
  import type { Snapshot } from "../lib/api";
  import { api } from "../lib/api";

  let { snap }: { snap: Snapshot } = $props();

  const links = [
    { label: "Website", url: "https://servertv.yarmiplay.com/", hint: "Downloads and the setup guide" },
    { label: "YarmiplayTV", url: "https://tv.yarmiplay.com/", hint: "The watch-together app this server is made for" },
    { label: "Source code", url: "https://github.com/Yarmiplay/YarmiplayServerTV", hint: "Report problems and follow development on GitHub" },
  ];
</script>

<div class="stack">
  <header>
    <h1>About</h1>
  </header>

  <section class="card intro">
    <img src={logo} alt="" width="56" height="56" />
    <div>
      <h2>YarmiplayServerTV</h2>
      <p class="muted">Version {snap.version}</p>
    </div>
  </section>

  <section class="card">
    <h2>Links</h2>
    <div class="links">
      {#each links as l (l.url)}
        <button class="link" onclick={() => api.openUrl(l.url)}>
          <span class="label">{l.label}</span>
          <span class="hint">{l.hint}</span>
        </button>
      {/each}
    </div>
  </section>

  <section class="card">
    <h2>License</h2>
    <p class="sub">
      Free software under the GNU General Public License, version 3 or later. Jellyfin is made by the Jellyfin project
      and keeps its own license. The Syncplay server works with Syncplay 1.7 clients.
    </p>
  </section>
</div>

<style>
  header h1 {
    font-size: 1.6em;
  }
  .intro {
    display: flex;
    align-items: center;
    gap: 16px;
  }
  .intro h2 {
    margin: 0;
  }
  .intro p {
    margin: 2px 0 0;
  }
  .links {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-top: 10px;
  }
  .link {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    text-align: left;
    padding: 10px 14px;
  }
  .label {
    color: var(--accent);
    font-weight: 600;
  }
  .hint {
    color: var(--muted);
    font-size: 0.9em;
  }
  .card .sub {
    margin: 8px 0 0;
  }
</style>
