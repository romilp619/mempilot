// Fake backend so the UI can be previewed in a plain browser (npm run dev). Never used in the app.
const GB = 1024 ** 3;
const MB = 1024 ** 2;
const subs: Record<string, ((p: any) => void)[]> = {};
const settings = {
  areas: ["workingSets", "modifiedPageList", "standbyLow", "registryCache"],
  excluded: ["audiodg.exe", "dwm.exe"], skipForeground: true,
  autoBelowPct: 0, autoEveryHours: 0, autoCooldownMin: 10,
  standbyCleaner: false, standbyMinMb: 1024, standbyFreeBelowMb: 1024, standbyCooldownSec: 60, standbyLowOnly: false,
  hotkeyEnabled: false, hotkey: "Ctrl+Shift+M", closeToTray: true, startMinimized: false, alwaysOnTop: false,
  lowPriority: true, trayShowUsage: true, middleClickOptimize: true, warnPct: 80, dangerPct: 90,
};
let standby = 3.1 * GB;
function stats() {
  const total = 15.1 * GB;
  const used = 10.8 * GB + Math.random() * 0.2 * GB;
  const free = total - used - standby;
  const by = [0.4, 0.2, 0.3, 0.1, 0.2, 1.3, 0.5, 0.1].map((f) => (f / 3.1) * standby);
  return {
    total, available: total - used, used, usedPct: (used / total) * 100,
    commitUsed: 17.2 * GB, commitLimit: 24.1 * GB, systemCache: 1.9 * GB,
    lists: { zero: free * 0.3, free: free * 0.7, modified: 420 * MB, modifiedNoWrite: 0, standby, standbyLow: by[0], standbyByPriority: by },
  };
}
setInterval(() => (subs["stats"] ?? []).forEach((f) => f(stats())), 1000);

export const mock = {
  listen<T>(ev: string, cb: (p: T) => void) {
    (subs[ev] ??= []).push(cb);
    return Promise.resolve(() => {});
  },
  async invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    const r = (v: unknown) => v as T;
    switch (cmd) {
      case "get_snapshot":
        return r({ settings, elevated: true, history: [], standby: { runs: 0, released: 0, lastAt: 0, lastReleased: 0 }, stats: stats(), version: "0.1.0" });
      case "run_optimize": {
        const before = stats();
        await new Promise((ok) => setTimeout(ok, 900));
        standby = 0.6 * GB;
        const after = stats();
        const results = ((args?.areas as string[]) ?? []).map((a) => ({ area: a, ok: true, detail: a === "workingSets" ? "212 processes trimmed, 3 skipped, 14 protected" : "done", ms: 40 }));
        const rep = { at: Date.now(), trigger: "manual", ms: 1300, results, before, after };
        (subs["optimized"] ?? []).forEach((f) => f(rep));
        return r(rep);
      }
      case "get_processes":
        return r(["chrome.exe", "Code.exe", "brave.exe", "Discord.exe", "explorer.exe", "dwm.exe", "MsMpEng.exe", "steam.exe"].map((name, i) => ({ pid: 1000 + i * 412, name, workingSet: (900 - i * 100) * MB, private: (700 - i * 80) * MB })));
      case "get_volumes":
        return r([{ root: "C:\\", label: "OS", fs: "NTFS", kind: "Fixed", total: 951 * GB, free: 212 * GB }]);
      case "get_physical_disks":
        return r([{ FriendlyName: "WD PC SN5000S SDEQNSJ-1T00-1002", MediaType: "SSD", BusType: "NVMe", HealthStatus: "Healthy", OperationalStatus: "OK", Size: 1024209543168 }]);
      case "get_startup":
        return r(false);
      default:
        return r(undefined);
    }
  },
};
