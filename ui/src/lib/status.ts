import type { Snapshot } from "./api";
import { formatBytes } from "./api";

export type Pill = { kind: "ok" | "warn" | "err" | "busy" | ""; text: string };

export function syncplayPill(s: Snapshot): Pill {
  if (s.syncplay.running) {
    const n = s.syncplay.users;
    return { kind: "ok", text: n === 0 ? "Running" : n === 1 ? "1 user" : `${n} users` };
  }
  if (s.syncplay.error) return { kind: "err", text: "Error" };
  return { kind: "", text: "Off" };
}

export function jellyfinPill(s: Snapshot): Pill {
  const j = s.jellyfin;
  switch (j.phase) {
    case "running":
      return { kind: "ok", text: "Running" };
    case "downloading": {
      const p = j.progress;
      const pct = p && p.total ? Math.floor((p.downloaded / p.total) * 100) : 0;
      return { kind: "busy", text: p ? `Downloading ${pct}% of ${formatBytes(p.total)}` : "Downloading" };
    }
    case "installing":
      return { kind: "busy", text: "Installing" };
    case "starting":
      return { kind: "busy", text: "Starting" };
    case "stopping":
      return { kind: "busy", text: "Stopping" };
    case "error":
      return { kind: "err", text: "Error" };
    default:
      return { kind: "", text: "Off" };
  }
}

export function tlsPill(s: Snapshot): Pill {
  const t = s.tls;
  switch (t.phase) {
    case "ready":
      return t.error
        ? { kind: "warn", text: "Renewal failed" }
        : { kind: t.staging ? "warn" : "ok", text: t.staging ? "Test certificate" : "Certificate active" };
    case "issuing":
      return { kind: "busy", text: "Getting certificate" };
    case "incomplete":
      return { kind: "warn", text: "Needs setup" };
    case "error":
      return { kind: "err", text: "Error" };
    default:
      return { kind: "", text: "Off" };
  }
}

export function upnpPill(s: Snapshot): Pill {
  const u = s.upnp;
  if (!u.active) return { kind: "", text: "Off" };
  if (u.error || u.mappings.some((m) => m.state === "error")) return { kind: "err", text: "Not forwarded" };
  if (u.doubleNat) return { kind: "warn", text: "Double NAT" };
  return { kind: "ok", text: "Forwarded" };
}

export function updatePill(s: Snapshot): Pill {
  const u = s.update;
  if (u.store) return { kind: "", text: "Microsoft Store" };
  if (!u.supported) return { kind: "", text: "Development build" };
  switch (u.phase) {
    case "checking":
      return { kind: "busy", text: "Checking" };
    case "downloading":
      return { kind: "busy", text: u.total ? `Downloading ${Math.floor((u.downloaded / u.total) * 100)}%` : "Downloading" };
    case "ready":
      return { kind: "warn", text: `Update ${u.version} ready` };
    case "installing":
      return { kind: "busy", text: "Installing" };
    case "upToDate":
      return { kind: "ok", text: "Up to date" };
    case "error":
      return { kind: "err", text: "Update failed" };
    default:
      return { kind: "", text: "Not checked yet" };
  }
}
