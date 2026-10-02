import { invoke } from "@tauri-apps/api/core";

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

export interface Settings {
  syncplay: SyncplaySettings;
  jellyfin: JellyfinSettings;
  tls: TlsSettings;
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
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  openFolder: (which: "data" | "jellyfin-logs") => invoke<void>("open_folder", { which }),
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
