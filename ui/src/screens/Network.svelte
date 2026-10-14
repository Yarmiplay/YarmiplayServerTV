<script lang="ts">
  import type { Snapshot } from "../lib/api";
  import { api, formatDate } from "../lib/api";
  import { store } from "../lib/store.svelte";
  import { useDraft } from "../lib/draft.svelte";
  import { mappingKind, mappingLabel, tlsPill, upnpPill } from "../lib/status";
  import Toggle from "../lib/Toggle.svelte";
  import Address from "../lib/Address.svelte";

  let { snap }: { snap: Snapshot } = $props();

  const tlsForm = useDraft(() => ({
    duckdnsDomain: snap.settings.tls.duckdnsDomain,
    email: snap.settings.tls.email,
    lanDomain: snap.settings.tls.lanDomain,
  }));
  let testing = $state(false);
  let testResult = $state<{ ok: boolean; text: string } | null>(null);

  async function testLan() {
    testing = true;
    testResult = null;
    try {
      testResult = { ok: true, text: await api.testLanName(tlsForm.draft.lanDomain) };
    } catch (e) {
      testResult = { ok: false, text: String(e) };
    }
    testing = false;
  }

  // The browser client: tv.yarmiplay.com/watch joins over HTTPS with the DuckDNS name.
  let room = $state("lobby");
  const fqdn = (d: string) => {
    const name = d.trim().toLowerCase().replace(/\.$/, "");
    return name.endsWith(".duckdns.org") ? name : `${name}.duckdns.org`;
  };
  const browserReady = $derived(
    snap.settings.tls.enabled && snap.tls.phase === "ready" && !snap.settings.syncplay.vanillaMode && !!snap.tls.host,
  );
  const watchUrl = $derived.by(() => {
    if (!browserReady || !snap.tls.host) return null;
    const r = encodeURIComponent(room.trim() || "lobby");
    const lan = snap.settings.tls.lanDomain.trim() ? `&lan=${fqdn(snap.settings.tls.lanDomain)}` : "";
    return `https://tv.yarmiplay.com/watch/#join=${snap.tls.host}:${snap.settings.syncplay.port}/${r}${lan}`;
  });
  let token = $state("");
  let showToken = $state(false);

  const tls = $derived(tlsPill(snap));
  const upnp = $derived(upnpPill(snap));

  async function saveToken() {
    const next = await store.run(() => api.setDuckdnsToken(token), "DuckDNS token saved");
    if (next) {
      store.snap = next;
      token = "";
    }
  }

  async function removeToken() {
    const next = await store.run(() => api.setDuckdnsToken(null), "DuckDNS token removed");
    if (next) store.snap = next;
  }

</script>

<div class="stack">
  <header>
    <h1>Network</h1>
    <p class="muted">
      Everything here is optional. On your own network, friends can always connect with the LAN address without
      encryption; YarmiplayTV finds this PC at home and connects encrypted on its own.
    </p>
  </header>

  <section class="card">
    <div class="spread">
      <h2>DuckDNS and HTTPS</h2>
      <span class="pill {tls.kind}">{tls.text}</span>
    </div>
    <p class="sub">
      A free <a href="https://www.duckdns.org" onclick={(e) => { e.preventDefault(); api.openUrl("https://www.duckdns.org"); }}>DuckDNS</a>
      name that follows your home IP, with a Let's Encrypt certificate for encrypted Syncplay (TLS) and Jellyfin (HTTPS).
    </p>
    <Toggle
      label="Use DuckDNS and HTTPS"
      checked={snap.settings.tls.enabled}
      onchange={(v) => store.save((s) => (s.tls.enabled = v))}
    />
    <div class="fields">
      <label class="field">DuckDNS domain<input bind:value={tlsForm.draft.duckdnsDomain} placeholder="myname.duckdns.org" /></label>
      <label class="field">Email for Let's Encrypt (optional)<input type="email" bind:value={tlsForm.draft.email} placeholder="Expiry notices" /></label>
      <label class="field">
        LAN name for browsers (optional)
        <input bind:value={tlsForm.draft.lanDomain} placeholder="myname-lan.duckdns.org" />
      </label>
    </div>
    <p class="muted note">
      A second DuckDNS name (on the same account) that this PC keeps pointed at its address on your network, and that
      goes on the same certificate. Browsers can't check a certificate when connecting to an IP address, so this lets
      people at home watch in a browser over your network instead of through the internet. Some routers block DNS
      answers with private addresses ("DNS rebinding protection") and need an exception for this name.
      {#if tlsForm.draft.lanDomain.trim()}
        <button class="small" disabled={testing} onclick={testLan}>{testing ? "Testing…" : "Test"}</button>
      {/if}
    </p>
    {#if testResult}<div class="notice {testResult.ok ? 'ok' : 'warn'}">{testResult.text}</div>{/if}
    <div class="row buttons">
      <button class="primary" disabled={!tlsForm.dirty} onclick={() => store.save((s) => Object.assign(s.tls, tlsForm.draft), "Saved")}>Save</button>
      <button disabled={!tlsForm.dirty} onclick={() => tlsForm.reset()}>Revert</button>
    </div>

    <label class="field token">
      DuckDNS token
      <span class="row">
        <input
          type={showToken ? "text" : "password"}
          bind:value={token}
          autocomplete="off"
          placeholder={snap.duckdnsTokenSet ? "Saved. Enter a new token to replace it" : "From duckdns.org after signing in"}
          style="flex:1"
        />
        <button class="small" onclick={() => (showToken = !showToken)}>{showToken ? "Hide" : "Show"}</button>
        <button class="small primary" disabled={!token.trim()} onclick={saveToken}>Save token</button>
        {#if snap.duckdnsTokenSet}<button class="small danger" onclick={removeToken}>Remove</button>{/if}
      </span>
      <span class="hint">Stored in your system's credential store, never shown again.</span>
    </label>

    <div class="toggles">
      <Toggle
        label="Test certificates (Let's Encrypt staging)"
        hint="For trying things out: browsers and clients won't trust these certificates"
        checked={snap.settings.tls.staging}
        onchange={(v) => store.save((s) => (s.tls.staging = v))}
      />
      <Toggle
        label="Publish IPv6 too"
        hint="Devices with IPv6 reach this PC by name directly, also at home where the router may not loop the IPv4 address back. From outside, your router's IPv6 firewall decides."
        checked={snap.settings.tls.ipv6}
        onchange={(v) => store.save((s) => (s.tls.ipv6 = v))}
      />
    </div>

    {#if snap.tls.phase !== "off"}
      <div class="kv">
        {#if snap.tls.host}<span>Domain</span><code>{snap.tls.host}</code>{/if}
        {#if snap.tls.notAfter}
          <span>Expires</span><span>{formatDate(snap.tls.notAfter)}</span>
          <span>Renews</span><span>{formatDate(snap.tls.renewAt)}</span>
        {/if}
        {#if snap.tls.duckdnsIp}<span>DuckDNS points to</span><code>{snap.tls.duckdnsIp}</code>{/if}
        {#if snap.tls.lanHost}
          <span>LAN name</span><code>{snap.tls.lanHost}{snap.tls.lanIp ? ` → ${snap.tls.lanIp}` : ""}</code>
        {/if}
        {#if snap.settings.tls.ipv6}
          <span>DuckDNS IPv6</span>
          {#if snap.tls.duckdnsIpv6}<code>{snap.tls.duckdnsIpv6}</code>{:else}<span class="muted"
              >No public IPv6 on this network</span
            >{/if}
        {/if}
      </div>
      {#if !snap.settings.tls.ipv6 && snap.tls.duckdnsIpv6}
        <div class="notice warn">
          DuckDNS still has the IPv6 address {snap.tls.duckdnsIpv6}. Clear it on duckdns.org if it shouldn't be used.
        </div>
      {/if}
      {#if snap.tls.error}<div class="notice {snap.tls.phase === 'ready' ? 'warn' : 'err'}">{snap.tls.error}</div>{/if}
      {#if snap.tls.phase === "ready" || snap.tls.phase === "error"}
        <div class="row buttons">
          <button onclick={() => store.run(() => api.renewCertificate(), "Requesting a new certificate")}>Renew now</button>
        </div>
      {/if}
    {/if}
  </section>

  <section class="card">
    <h2>Watch in a browser</h2>
    {#if watchUrl}
      <p class="sub">
        Anyone with this link can open YarmiplayTV in their browser and join the room (they still need your approval or
        the password, as on any device).
      </p>
      <label class="field room">Room<input bind:value={room} placeholder="lobby" /></label>
      <Address label="Link" value={watchUrl} link />
    {:else}
      <p class="sub">
        Browsers can only join over HTTPS with a real certificate: set up DuckDNS and HTTPS above{snap.settings.syncplay
          .vanillaMode
          ? ", and turn off vanilla Syncplay mode on the Syncplay page"
          : ""}. Then a link to tv.yarmiplay.com/watch shows here.
      </p>
    {/if}
  </section>

  <section class="card">
    <div class="spread">
      <h2>Port forwarding (UPnP)</h2>
      <span class="pill {upnp.kind}">{upnp.text}</span>
    </div>
    <p class="sub">
      Asks your router to forward the server ports to this PC. Off by default; you can also forward the ports by hand in your
      router's settings.
    </p>
    <Toggle
      label="Syncplay"
      hint={`TCP ${snap.settings.syncplay.port}`}
      checked={snap.settings.syncplay.upnp}
      onchange={(v) => store.save((s) => (s.syncplay.upnp = v))}
    />
    <Toggle
      label="Jellyfin"
      hint={`TCP ${snap.settings.jellyfin.httpPort}${snap.jellyfin.https ? ` and ${snap.settings.jellyfin.httpsPort}` : ""}`}
      checked={snap.settings.jellyfin.upnp}
      onchange={(v) => store.save((s) => (s.jellyfin.upnp = v))}
    />
    {#if snap.settings.plex.enabled}
      <Toggle
        label="Plex"
        hint={`TCP ${snap.settings.plex.port}`}
        checked={snap.settings.plex.upnp}
        onchange={(v) => store.save((s) => (s.plex.upnp = v))}
      />
    {/if}
    {#if snap.upnp.active}
      <div class="kv">
        {#if snap.upnp.localIp}<span>This PC</span><code>{snap.upnp.localIp}</code>{/if}
        {#if snap.upnp.externalIp}<span>Public IP</span><code>{snap.upnp.externalIp}</code>{/if}
      </div>
      {#if snap.upnp.error}<div class="notice err">{snap.upnp.error}</div>{/if}
      {#if snap.upnp.doubleNat}
        <div class="notice warn">
          Your router's internet address is private (double NAT or carrier-grade NAT). Forwards on this router may not be
          reachable from the internet.
        </div>
      {/if}
      {#if snap.upnp.mappings.length}
        <ul class="maps">
          {#each snap.upnp.mappings as m (m.port)}
            <li class="spread">
              <span>{m.label} <span class="muted">TCP {m.port}</span></span>
              <span class="pill {mappingKind[m.state]}" title={m.error ?? ""}>{mappingLabel[m.state]}</span>
            </li>
            {#if m.error}<li class="muted err-line">{m.error}</li>{/if}
          {/each}
        </ul>
      {/if}
    {:else}
      <p class="muted note">Mappings are only made for servers that are switched on.</p>
    {/if}
  </section>

  <section class="card">
    <h2>Addresses</h2>
    <div class="kv">
      <span>This PC on your network</span><code>{snap.addresses.lanIp ?? "unknown"}</code>
      <span>Public name</span><code>{snap.addresses.publicHost ?? "unknown"}</code>
    </div>
  </section>
</div>

<style>
  h1 {
    font-size: 1.6em;
  }
  header p {
    margin: 4px 0 0;
  }
  .fields {
    margin-top: 10px;
  }
  .buttons {
    margin-top: 12px;
  }
  .token {
    margin-top: 16px;
  }
  .hint {
    font-size: 0.88em;
  }
  .toggles {
    margin-top: 10px;
  }
  .kv {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 8px 16px;
    align-items: center;
    justify-items: start;
    color: var(--muted);
    font-size: 0.93em;
    margin-top: 12px;
  }
  .notice {
    margin-top: 10px;
  }
  .maps {
    list-style: none;
    margin: 12px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .err-line {
    font-size: 0.88em;
  }
  .note {
    font-size: 0.9em;
  }
  .room {
    max-width: 240px;
    margin-bottom: 8px;
  }
</style>
