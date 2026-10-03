import { api, BROWSER_OFF, clone, errorText, subscribe, type LogLine, type Settings, type Snapshot } from "./api";

const MAX_LOGS = 2000;

class AppStore {
  snap = $state<Snapshot | null>(null);
  logs = $state<LogLine[]>([]);
  toast = $state<{ text: string; kind: "ok" | "err" } | null>(null);
  /** Set while a browser tab can't get live updates from the app. */
  connection = $state<string | null>(null);
  #toastTimer: ReturnType<typeof setTimeout> | undefined;

  async init() {
    await subscribe({
      status: (snap) => (this.snap = snap),
      log: (line) => {
        const next = this.logs.length >= MAX_LOGS ? this.logs.slice(-MAX_LOGS + 1) : this.logs.slice();
        next.push(line);
        this.logs = next;
      },
      resync: () => void this.reload().catch(() => {}),
      connection: (problem) =>
        (this.connection = problem && this.snap?.settings.browser.enabled === false ? BROWSER_OFF : problem),
    });
    await this.reload();
  }

  async reload() {
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
