import { listen } from "@tauri-apps/api/event";
import { api, clone, errorText, type LogLine, type Settings, type Snapshot } from "./api";

const MAX_LOGS = 2000;

class AppStore {
  snap = $state<Snapshot | null>(null);
  logs = $state<LogLine[]>([]);
  toast = $state<{ text: string; kind: "ok" | "err" } | null>(null);
  #toastTimer: ReturnType<typeof setTimeout> | undefined;

  async init() {
    await listen<Snapshot>("status", (e) => (this.snap = e.payload));
    await listen<LogLine>("log", (e) => {
      const next = this.logs.length >= MAX_LOGS ? this.logs.slice(-MAX_LOGS + 1) : this.logs.slice();
      next.push(e.payload);
      this.logs = next;
    });
    this.snap = await api.getState();
    this.logs = await api.getLogs();
  }

  notify(text: string, kind: "ok" | "err" = "ok") {
    this.toast = { text, kind };
    clearTimeout(this.#toastTimer);
    this.#toastTimer = setTimeout(() => (this.toast = null), kind === "err" ? 7000 : 3000);
  }

  /** Apply a change to a copy of the saved settings. Returns false on error. */
  async save(change: (s: Settings) => void, okText?: string): Promise<boolean> {
    if (!this.snap) return false;
    const next = clone(this.snap.settings);
    change(next);
    try {
      this.snap = await api.updateSettings(next);
      if (okText) this.notify(okText);
      return true;
    } catch (e) {
      this.notify(errorText(e), "err");
      return false;
    }
  }

  /** Run an action and toast its error. */
  async run<T>(action: () => Promise<T>, okText?: string): Promise<T | undefined> {
    try {
      const r = await action();
      if (okText) this.notify(okText);
      return r;
    } catch (e) {
      this.notify(errorText(e), "err");
      return undefined;
    }
  }
}

export const store = new AppStore();
