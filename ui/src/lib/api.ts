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

/**
 * Who may join: anyone, the password for everyone, the password or an approved device, or approved
 * devices only.
 */
export type SyncplayAccess = "open" | "passwordOnly" | "password" | "approved";

export function usesPassword(access: SyncplayAccess): boolean {
  return access === "passwordOnly" || access === "password";
}

export interface SyncplaySettings {
  enabled: boolean;
  port: number;
  access: SyncplayAccess;
  /** Used in the two password modes only. */
  password: string;
  /** Welcome message shown to clients when they join. */
  motd: string;
  isolateRooms: boolean;
  disableChat: boolean;
  disableReady: boolean;
  maxChatMessageLength: number;
  maxUsernameLength: number;
  upnp: boolean;
  /** Exactly like the official server: no YarmiplayTV extensions, relay or Jellyfin sharing. */
  vanillaMode: boolean;
  fileRelay: boolean;
  relayCacheGb: number;
  /** Web pages allowed to join from a browser and read the relay cross-origin. */
  webOrigins: string[];
  /** Managed rooms (`+name:HASH`), where only operators control playback. */
  managedRooms: boolean;
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
  shareWithSyncplay: boolean;
  /** Managed by the app. */
  guestUserId: string | null;
}

export interface PlexInvite {
  plexUserId: string;
  username: string;
  syncplayName: string;
  shareId: string;
  /** Unix seconds. */
  at: number;
}

/** A Plex Media Server the user installed; the app never installs or runs it. */
export interface PlexSettings {
  enabled: boolean;
  port: number;
  upnp: boolean;
  shareWithSyncplay: boolean;
  /** Library keys shared with Syncplay users; null shares every library. */
  sharedSections: string[] | null;
  /** Managed by the app. */
  clientId: string;
  accountUser: string | null;
  machineId: string | null;
  invited: PlexInvite[];
  savedCustomConnections: string | null;
}

export interface TlsSettings {
  enabled: boolean;
  duckdnsDomain: string;
  email: string;
  staging: boolean;
  /** Publish this PC's public IPv6 address on DuckDNS too. */
  ipv6: boolean;
  /** A second DuckDNS name kept pointing at this PC's LAN address, for browsers at home. */
  lanDomain: string;
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
  plex: PlexSettings;
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
  /** Who updates this copy instead of the built-in updater: "microsoft-store", "snap", or a distribution's name. */
  managedBy: string | null;
}

export interface RoomInfo {
  name: string;
  users: string[];
  paused: boolean;
  /** A managed room (`+name:HASH`). */
  controlled: boolean;
  /** Its operators. */
  controllers: string[];
}

/** A managed room made on the Syncplay page. */
export interface ManagedRoom {
  room: string;
  password: string;
}

/** "movie" for the managed room "+movie:1A2B3C4D5E6F". */
export function roomBaseName(name: string): string {
  const m = /^\+(.*):(\w{12})$/u.exec(name);
  return m ? m[1] : name;
}

export interface RelayActivity {
  room: string;
  name: string;
  size: number;
  cachedBytes: number;
  sources: number;
  readers: number;
  /** Bytes per second arriving from the sources. */
  rate: number;
}

export interface RelayStatus {
  enabled: boolean;
  cacheBytes: number;
  cacheLimitBytes: number;
  active: RelayActivity[];
}

export interface PendingDevice {
  fingerprint: string;
  name: string;
  username: string;
  ip: string;
  /** Unix seconds. */
  requestedAt: number;
  /** Still connected and waiting. */
  connected: boolean;
}

export interface ApprovedDevice {
  fingerprint: string;
  publicKey: string;
  name: string;
  approvedAt: number;
  lastSeen: number;
  lastUsername: string;
}

export interface DevicesStatus {
  pending: PendingDevice[];
  approved: ApprovedDevice[];
}

export interface SyncplayStatus {
  running: boolean;
  port: number | null;
  error: string | null;
  users: number;
  rooms: RoomInfo[];
  tls: boolean;
  relay: RelayStatus;
  devices: DevicesStatus;
}

export interface ShareStatus {
  /** Offered to Syncplay users right now. */
  active: boolean;
  error: string | null;
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
  serverId: string | null;
}

export interface PlexStatus {
  phase: "off" | "notInstalled" | "notRunning" | "unclaimed" | "running";
  /** null when it can't be known (the Snap can't see installed programs). */
  installed: boolean | null;
  version: string | null;
  machineId: string | null;
  serverName: string | null;
}

export interface PlexAccount {
  signedIn: boolean;
  /** A code waiting to be entered at plex.tv/link. */
  link: { code: string; expiresAt: number } | null;
  /** The running Plex belongs to the linked account; null until checked. */
  ownsServer: boolean | null;
  error: string | null;
}

export interface TlsStatus {
  phase: "off" | "incomplete" | "issuing" | "ready" | "error";
  host: string | null;
  notAfter: number | null;
  renewAt: number | null;
  staging: boolean;
  error: string | null;
  duckdnsIp: string | null;
  duckdnsIpv6: string | null;
  /** The LAN name, and the LAN address DuckDNS has for it. */
  lanHost: string | null;
  lanIp: string | null;
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
  plexLocal: string | null;
  plexLan: string | null;
  plexPublic: string | null;
}

export interface Snapshot {
  version: string;
  /** The Syncplay version the server reports, e.g. "1.7.6". */
  syncplayVersion: string;
  settings: Settings;
  duckdnsTokenSet: boolean;
  jellyfinSignedIn: boolean;
  syncplay: SyncplayStatus;
  jellyfin: JellyfinStatus;
  tls: TlsStatus;
  upnp: UpnpStatus;
  addresses: Addresses;
  update: UpdateStatus;
  jellyfinShare: ShareStatus;
  plex: PlexStatus;
  plexAccount: PlexAccount;
  plexShare: ShareStatus;
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

export interface PlexLibrary {
  key: string;
  name: string;
  /** movie | show | artist | photo */
  kind: string;
  locations: string[];
}

export const api = {
  getState: () => invoke<Snapshot>("get_state"),
  updateSettings: (settings: Settings) => invoke<Snapshot>("update_settings", { settings }),
  setDuckdnsToken: (token: string | null) => invoke<Snapshot>("set_duckdns_token", { token }),
  renewCertificate: () => invoke<void>("renew_certificate"),
  testLanName: (name: string) => invoke<string>("tls_test_lan_name", { name }),
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
  plexLinkStart: () => invoke<Snapshot>("plex_link_start"),
  plexLinkCancel: () => invoke<Snapshot>("plex_link_cancel"),
  plexLogout: () => invoke<Snapshot>("plex_logout"),
  plexRefresh: () => invoke<void>("plex_refresh"),
  plexLibraries: () => invoke<PlexLibrary[]>("plex_libraries"),
  plexAddLibrary: (name: string, kind: string, path: string) => invoke<void>("plex_add_library", { name, kind, path }),
  plexRemoveLibrary: (key: string) => invoke<void>("plex_remove_library", { key }),
  plexAddPath: (key: string, path: string) => invoke<void>("plex_add_path", { key, path }),
  plexRemovePath: (key: string, path: string) => invoke<void>("plex_remove_path", { key, path }),
  plexRescan: () => invoke<void>("plex_rescan"),
  plexRemoveInvite: (shareId: string) => invoke<Snapshot>("plex_remove_invite", { shareId }),
  clearRelayCache: () => invoke<Snapshot>("clear_relay_cache"),
  approveDevice: (fingerprint: string) => invoke<Snapshot>("syncplay_device_approve", { fingerprint }),
  denyDevice: (fingerprint: string) => invoke<Snapshot>("syncplay_device_deny", { fingerprint }),
  removeDevice: (fingerprint: string) => invoke<Snapshot>("syncplay_device_remove", { fingerprint }),
  renameDevice: (fingerprint: string, name: string) =>
    invoke<Snapshot>("syncplay_device_rename", { fingerprint, name }),
  createManagedRoom: (name: string) => invoke<ManagedRoom>("syncplay_create_managed_room", { name }),
  resetRoomSalt: () => invoke<Snapshot>("syncplay_reset_room_salt"),
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

export function formatDateTime(unix: number | null): string {
  if (!unix) return "–";
  return new Date(unix * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

export function formatBytes(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)} GB`;
  return `${(n / 1e6).toFixed(0)} MB`;
}
