import { invoke as tauriInvoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";

/** False when the UI is opened in a regular browser (served by the app on 127.0.0.1). */
export const inApp = isTauri();

const NOT_SIGNED_IN =
  "This browser isn't signed in to YarmiplayServerTV. Use \"Open in Browser\" in the tray icon menu to open the control panel.";
const UNREACHABLE = "Could not reach YarmiplayServerTV. Make sure the app is running.";
export const BROWSER_OFF =
  "Browser access was turned off. To use the control panel here again, turn it back on from the Dashboard in the YarmiplayServerTV window, then choose \"Open in Browser\" in the tray icon menu.";

async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (inApp) return tauriInvoke<T>(command, args);
  let res: Response;
  try {
    res = await fetch(`/api/invoke/${command}`, {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-YSTV": "1" },
      body: JSON.stringify(args ?? {}),
    });
  } catch {
    throw UNREACHABLE;
  }
  if (res.status === 401) throw NOT_SIGNED_IN;
  const body = await res.json().catch(() => null);
  if (!res.ok) throw typeof body === "string" ? body : `Request failed (HTTP ${res.status})`;
  return body as T;
}

export interface EventHandlers {
  status: (snap: Snapshot) => void;
  log: (line: LogLine) => void;
  /** Events may have been missed; reload the full state. */
  resync: () => void;
  connection: (problem: string | null) => void;
}

export async function subscribe(on: EventHandlers): Promise<void> {
  if (inApp) {
    await listen<Snapshot>("status", (e) => on.status(e.payload));
    await listen<LogLine>("log", (e) => on.log(e.payload));
    return;
  }
  const events = new EventSource("/api/events");
  let opened = false;
  events.addEventListener("status", (e) => on.status(JSON.parse(e.data)));
  events.addEventListener("log", (e) => on.log(JSON.parse(e.data)));
  events.addEventListener("resync", () => on.resync());
  events.onopen = () => {
    on.connection(null);
    if (opened) on.resync();
    opened = true;
  };
  events.onerror = () => {
    if (events.readyState === EventSource.CLOSED) on.connection(NOT_SIGNED_IN);
    else on.connection("Lost the connection to YarmiplayServerTV. Reconnecting…");
  };
}

export function copyText(text: string): Promise<void> {
  return inApp ? writeText(text) : navigator.clipboard.writeText(text);
}

export interface SyncplaySettings {
  enabled: boolean;
  port: number;
  password: string;
  motd: string;
  isolateRooms: boolean;
  disableChat: boolean;
  disableReady: boolean;
  maxChatMessageLength: number;
  maxUsernameLength: number;
  upnp: boolean;
}

export interface JellyfinSettings {
  enabled: boolean;
  httpPort: number;
  httpsPort: number;
  upnp: boolean;
  setupComplete: boolean;
  adminUser: string | null;
  adminUserId: string | null;
  deviceId: string;
}

export interface TlsSettings {
  enabled: boolean;
  duckdnsDomain: string;
  email: string;
  staging: boolean;
}

export interface BrowserSettings {
  enabled: boolean;
}

export interface UpdateSettings {
  auto: boolean;
}

export interface Settings {
  syncplay: SyncplaySettings;
  jellyfin: JellyfinSettings;
  tls: TlsSettings;
  browser: BrowserSettings;
  updates: UpdateSettings;
}

export interface UpdateStatus {
  phase: "idle" | "checking" | "upToDate" | "downloading" | "ready" | "installing" | "error";
  version: string | null;
  notes: string | null;
  downloaded: number;
  total: number | null;
  error: string | null;
  lastCheck: number | null;
  /** False for .msi and .deb installs, which need an administrator prompt to update. */
  unattended: boolean;
  supported: boolean;
  /** Who updates this copy instead of the built-in updater: "microsoft-store", "flathub", "snap", "aur", ... */
  managedBy: string | null;
}

export interface RoomInfo {
  name: string;
  users: string[];
  paused: boolean;
}

export interface SyncplayStatus {
  running: boolean;
  port: number | null;
  error: string | null;
  users: number;
  rooms: RoomInfo[];
  tls: boolean;
}

export interface JellyfinStatus {
  phase: "off" | "downloading" | "installing" | "starting" | "running" | "stopping" | "error";
  installedVersion: string | null;
  pinnedVersion: string;
  progress?: { downloaded: number; total: number; stage: string };
  error: string | null;
  pid: number | null;
  https: boolean;
  wizardCompleted: boolean | null;
  serverName: string | null;
}

export interface TlsStatus {
  phase: "off" | "incomplete" | "issuing" | "ready" | "error";
  host: string | null;
  notAfter: number | null;
  renewAt: number | null;
  staging: boolean;
  error: string | null;
  duckdnsIp: string | null;
}

export interface MappingStatus {
  port: number;
  label: string;
  state: "ok" | "manual" | "error";
  error: string | null;
}

export interface UpnpStatus {
  active: boolean;
  gateway: string | null;
  localIp: string | null;
  externalIp: string | null;
  doubleNat: boolean;
  mappings: MappingStatus[];
  error: string | null;
}

export interface Addresses {
  lanIp: string | null;
  publicHost: string | null;
  syncplayLan: string | null;
  syncplayPublic: string | null;
  jellyfinLocal: string | null;
  jellyfinLan: string | null;
  jellyfinPublic: string | null;
}

export interface Snapshot {
  version: string;
  settings: Settings;
  duckdnsTokenSet: boolean;
  jellyfinSignedIn: boolean;
  syncplay: SyncplayStatus;
  jellyfin: JellyfinStatus;
  tls: TlsStatus;
  upnp: UpnpStatus;
  addresses: Addresses;
  update: UpdateStatus;
}

export interface LogLine {
  seq: number;
  time: number;
  level: "error" | "warn" | "info" | "debug" | "trace";
  target: string;
  message: string;
}

export interface Library {
  name: string;
  collectionType: string | null;
  locations: string[];
  itemId: string | null;
}

export const api = {
  getState: () => invoke<Snapshot>("get_state"),
  updateSettings: (settings: Settings) => invoke<Snapshot>("update_settings", { settings }),
  setDuckdnsToken: (token: string | null) => invoke<Snapshot>("set_duckdns_token", { token }),
  renewCertificate: () => invoke<void>("renew_certificate"),
  jellyfinRetry: () => invoke<void>("jellyfin_retry"),
  jellyfinSetup: (serverName: string, username: string, password: string) =>
    invoke<Snapshot>("jellyfin_setup", { serverName, username, password }),
  jellyfinLogin: (username: string, password: string) => invoke<Snapshot>("jellyfin_login", { username, password }),
  jellyfinLogout: () => invoke<Snapshot>("jellyfin_logout"),
  jellyfinLibraries: () => invoke<Library[]>("jellyfin_libraries"),
  jellyfinAddLibrary: (name: string, collectionType: string, path: string) =>
    invoke<void>("jellyfin_add_library", { name, collectionType, path }),
  jellyfinRemoveLibrary: (name: string) => invoke<void>("jellyfin_remove_library", { name }),
  jellyfinAddPath: (library: string, path: string) => invoke<void>("jellyfin_add_path", { library, path }),
  jellyfinRemovePath: (library: string, path: string) => invoke<void>("jellyfin_remove_path", { library, path }),
  jellyfinRescan: () => invoke<void>("jellyfin_rescan"),
  getLogs: () => invoke<LogLine[]>("get_logs"),
  clearLogs: () => invoke<void>("clear_logs"),
  openUrl: async (url: string) => {
    if (inApp) return invoke<void>("open_url", { url });
    window.open(url, "_blank", "noopener,noreferrer");
  },
  openFolder: (which: "data" | "jellyfin-logs") => invoke<void>("open_folder", { which }),
  pickFolder: () => invoke<string | null>("pick_folder"),
  getAutostart: () => invoke<boolean>("get_autostart"),
  setAutostart: (enabled: boolean) => invoke<boolean>("set_autostart", { enabled }),
  openInBrowser: () => invoke<void>("open_in_browser"),
  checkForUpdate: () => invoke<void>("check_for_update"),
  installUpdate: () => invoke<void>("install_update"),
  quit: () => invoke<void>("quit_app"),
};

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}

export function clone<T>(v: T): T {
  return JSON.parse(JSON.stringify(v));
}

export function formatDate(unix: number | null): string {
  if (!unix) return "–";
  return new Date(unix * 1000).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

export function formatBytes(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)} GB`;
  return `${(n / 1e6).toFixed(0)} MB`;
}
