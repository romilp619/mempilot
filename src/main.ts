import { $, esc, fmt, signed, when, toast, invoke, listen } from "./lib";
import { renderCleaner, renderDupes, renderHealth } from "./pages";

type Area =
  | "workingSets" | "systemFileCache" | "modifiedFileCache" | "modifiedPageList"
  | "standbyLow" | "standby" | "combinePages" | "registryCache";

interface Lists { zero: number; free: number; modified: number; modifiedNoWrite: number; standby: number; standbyLow: number; standbyByPriority: number[] }
interface Stats { total: number; available: number; used: number; usedPct: number; commitUsed: number; commitLimit: number; systemCache: number; lists: Lists | null }
interface AreaResult { area: Area; ok: boolean; detail: string; ms: number }
interface Report { at: number; trigger: string; ms: number; results: AreaResult[]; before: Stats; after: Stats }
interface StandbyStatus { runs: number; released: number; lastAt: number; lastReleased: number }
interface Settings {
  areas: Area[]; excluded: string[]; skipForeground: boolean;
  autoBelowPct: number; autoEveryHours: number; autoCooldownMin: number;
  standbyCleaner: boolean; standbyMinMb: number; standbyFreeBelowMb: number; standbyCooldownSec: number; standbyLowOnly: boolean;
  hotkeyEnabled: boolean; hotkey: string; closeToTray: boolean; startMinimized: boolean; alwaysOnTop: boolean;
  lowPriority: boolean; trayShowUsage: boolean; middleClickOptimize: boolean; warnPct: number; dangerPct: number;
}
interface Snapshot { settings: Settings; elevated: boolean; history: Report[]; standby: StandbyStatus; stats: Stats; version: string }
interface Proc { pid: number; name: string; workingSet: number; private: number }

const AREAS: { id: Area; name: string; desc: string; tag: string; safe: boolean }[] = [
  { id: "workingSets", name: "Working sets", desc: "Asks apps to hand back pages they are not using. Pages come back when touched, so a busy app can hitch for a moment.", tag: "brief hitch", safe: false },
  { id: "modifiedPageList", name: "Modified page list", desc: "Writes changed pages to the page file so the RAM can be reused.", tag: "safe", safe: true },
  { id: "standbyLow", name: "Standby list (low priority)", desc: "Drops only the least useful cached pages.", tag: "safe", safe: true },
  { id: "standby", name: "Standby list (all)", desc: "Empties Windows' whole RAM cache. Frees the most, but apps reload data from disk.", tag: "slower reloads", safe: false },
  { id: "systemFileCache", name: "System file cache", desc: "Drops cached file data. The next file opens read from disk again.", tag: "slower reads", safe: false },
  { id: "modifiedFileCache", name: "Write file cache to disk", desc: "Writes pending file changes on each fixed drive now instead of a few seconds later.", tag: "safe", safe: true },
  { id: "combinePages", name: "Combine identical pages", desc: "Merges duplicate memory pages into one copy (Windows 10 and 11).", tag: "safe", safe: true },
  { id: "registryCache", name: "Registry cache", desc: "Writes pending registry changes to disk.", tag: "safe", safe: true },
];
const PRESETS: Record<string, Area[]> = {
  gentle: ["modifiedPageList", "standbyLow", "combinePages", "registryCache"],
  balanced: ["workingSets", "modifiedPageList", "standbyLow", "registryCache"],
  deep: ["workingSets", "systemFileCache", "modifiedFileCache", "modifiedPageList", "standby", "combinePages", "registryCache"],
};
const PRESET_NOTE: Record<string, string> = {
  gentle: "Nothing an app is using is touched. Good to run any time.",
  balanced: "Recommended. Trims idle apps and the least useful cache, keeps the rest of the cache.",
  deep: "Frees the most RAM. Expect a few seconds of slower app switching while caches refill.",
  custom: "Your own selection.",
};
const AREA_NAME = Object.fromEntries(AREAS.map((a) => [a.id, a.name])) as Record<Area, string>;

let snap: Snapshot;
let stats: Stats;
let busy = false;
let lastReport: Report | null = null;
const spark: { used: number; standby: number }[] = [];
let procs: Proc[] = [];

const trulyFree = (s: Stats) => (s.lists ? s.lists.zero + s.lists.free : NaN);

function currentPreset(areas: Area[]) {
  const key = [...areas].sort().join();
  for (const [k, v] of Object.entries(PRESETS)) if ([...v].sort().join() === key) return k;
  return "custom";
}

// ------------------------------------------------------------------ settings

let saveTimer = 0;
function saveSettings(immediate = false) {
  clearTimeout(saveTimer);
  const run = () =>
    invoke("save_settings", { settings: snap.settings }).catch((e) => toast(String(e)));
  if (immediate) run();
  else saveTimer = window.setTimeout(run, 350);
}

// ------------------------------------------------------------------ overview

function renderOverview() {
  const el = $("#tab-overview");
  const preset = currentPreset(snap.settings.areas);
  el.innerHTML = `
    <h1>Memory</h1>
    <p class="sub">What your RAM holds right now, and what each clean-up step would release.</p>
    <div class="card" id="memcard"></div>
    <div class="card">
      <h2>Optimize</h2>
      <div class="presets">
        ${["gentle", "balanced", "deep", "custom"].map((p) => `<button data-preset="${p}" class="${p === preset ? "on" : ""}">${p[0].toUpperCase() + p.slice(1)}</button>`).join("")}
      </div>
      <div class="muted small" style="margin:-4px 0 10px">${PRESET_NOTE[preset]}</div>
      <div class="areas">
        ${AREAS.map((a) => `
          <label class="area">
            <input type="checkbox" data-area="${a.id}" ${snap.settings.areas.includes(a.id) ? "checked" : ""}>
            <div><div class="t">${a.name}<span class="tag ${a.safe ? "safe" : "cost"}">${a.tag}</span></div><div class="d">${a.desc}</div></div>
          </label>`).join("")}
      </div>
      <div class="go">
        <button class="btn primary" id="go" ${busy ? "disabled" : ""}>${busy ? "Optimizing…" : "Optimize now"}</button>
        <span class="muted small">${snap.elevated ? "Running as administrator: every step is available." : "Most steps need administrator. Use Restart as administrator on the left."}</span>
      </div>
      <div id="result"></div>
    </div>
    <div class="card"><h2>History</h2><div class="history" id="history"></div></div>`;

  el.querySelectorAll<HTMLButtonElement>("[data-preset]").forEach((b) =>
    b.addEventListener("click", () => {
      const p = b.dataset.preset!;
      if (p === "custom") return;
      snap.settings.areas = [...PRESETS[p]];
      saveSettings();
      renderOverview();
    }),
  );
  el.querySelectorAll<HTMLInputElement>("[data-area]").forEach((c) =>
    c.addEventListener("change", () => {
      const id = c.dataset.area as Area;
      const s = new Set(snap.settings.areas);
      c.checked ? s.add(id) : s.delete(id);
      snap.settings.areas = AREAS.map((a) => a.id).filter((x) => s.has(x));
      saveSettings();
      renderOverview();
    }),
  );
  $("#go").addEventListener("click", runOptimize);
  renderMem();
  renderResult();
  renderHistory();
}

function renderMem() {
  const card = $("#memcard");
  if (!card || !stats) return;
  const s = stats;
  const l = s.lists;
  const pct = s.usedPct;
  const color = pct >= snap.settings.dangerPct ? "var(--bad)" : pct >= snap.settings.warnPct ? "var(--warn)" : "var(--text)";
  const segs = l
    ? [
        { k: "In use", v: Math.max(0, s.used - l.modified), c: "var(--inuse)" },
        { k: "Modified", v: l.modified, c: "var(--modified)" },
        { k: "Standby (cache)", v: l.standby, c: "var(--standby)" },
        { k: "Free", v: l.zero + l.free, c: "var(--free)" },
      ]
    : [
        { k: "Used", v: s.used, c: "var(--inuse)" },
        { k: "Available", v: s.available, c: "var(--free)" },
      ];
  card.innerHTML = `
    <div class="hero">
      <span class="pct" style="color:${color}">${pct.toFixed(0)}%</span>
      <span class="muted">${fmt(s.used)} used of ${fmt(s.total)}</span>
    </div>
    <div class="bar">${segs.map((g) => `<div title="${g.k}: ${fmt(g.v)}" style="width:${(g.v / s.total) * 100}%;background:${g.c}"></div>`).join("")}</div>
    <div class="legend">${segs.map((g) => `<span><i style="background:${g.c}"></i>${g.k} <b>${fmt(g.v)}</b></span>`).join("")}</div>
    <div class="tiles">
      <div class="tile"><div class="k">Available to apps</div><div class="v">${fmt(s.available)}</div></div>
      <div class="tile"><div class="k">Truly free (no cache)</div><div class="v">${l ? fmt(l.zero + l.free) : "—"}</div></div>
      <div class="tile"><div class="k">Standby cache</div><div class="v">${l ? fmt(l.standby) : "—"}</div></div>
      <div class="tile"><div class="k">Modified, waiting to be written</div><div class="v">${l ? fmt(l.modified) : "—"}</div></div>
      <div class="tile"><div class="k">Committed / limit</div><div class="v">${fmt(s.commitUsed)} <span class="muted small">/ ${fmt(s.commitLimit)}</span></div></div>
      <div class="tile"><div class="k">System file cache</div><div class="v">${fmt(s.systemCache)}</div></div>
    </div>
    <canvas class="spark" id="spark"></canvas>
    <div class="legend" style="margin-top:4px"><span><i style="background:var(--inuse)"></i>Used % (last 2 min)</span>${l ? `<span><i style="background:var(--standby)"></i>Standby %</span>` : ""}
    ${l ? "" : `<span>Run as administrator to see the standby and modified breakdown.</span>`}</div>`;
  drawSpark();
}

function drawSpark() {
  const c = $<HTMLCanvasElement>("#spark");
  if (!c) return;
  const dpr = devicePixelRatio || 1;
  const w = c.clientWidth, h = c.clientHeight;
  c.width = w * dpr; c.height = h * dpr;
  const g = c.getContext("2d")!;
  g.scale(dpr, dpr);
  g.strokeStyle = "#24303a"; g.lineWidth = 1;
  for (const f of [0, 0.5, 1]) { g.beginPath(); g.moveTo(0, 2 + f * (h - 4)); g.lineTo(w, 2 + f * (h - 4)); g.stroke(); }
  const line = (key: "used" | "standby", col: string) => {
    if (spark.length < 2) return;
    g.beginPath();
    spark.forEach((p, i) => {
      const x = (i / 119) * w + (w - ((spark.length - 1) / 119) * w);
      const y = 2 + (1 - p[key] / 100) * (h - 4);
      i ? g.lineTo(x, y) : g.moveTo(x, y);
    });
    g.strokeStyle = col; g.lineWidth = 2; g.lineJoin = "round"; g.stroke();
  };
  const css = getComputedStyle(document.documentElement);
  if (stats?.lists) line("standby", css.getPropertyValue("--standby"));
  line("used", css.getPropertyValue("--inuse"));
}

function reportSummary(r: Report) {
  const free = trulyFree(r.after) - trulyFree(r.before);
  const avail = r.after.available - r.before.available;
  return { free, avail };
}

function renderResult() {
  const el = $("#result");
  if (!el || !lastReport) return;
  const r = lastReport;
  const { free, avail } = reportSummary(r);
  el.innerHTML = `<div class="result">
    <div class="big">${Number.isNaN(free) ? "" : `Truly free <span class="${free >= 0 ? "pos" : "neg"}">${signed(free)}</span> · `}Available to apps <span class="${avail >= 0 ? "pos" : "neg"}">${signed(avail)}</span>
      <span class="muted small"> in ${(r.ms / 1000).toFixed(1)} s</span></div>
    <ul>${r.results.map((x) => `<li><span class="${x.ok ? "ok" : "no"}">${x.ok ? "✓" : "✕"}</span><span>${AREA_NAME[x.area]}</span><span class="muted">${esc(x.detail)}</span></li>`).join("")}</ul>
    ${Number.isNaN(free) ? "" : `<p class="muted small" style="margin:8px 0 0">"Available" already counts the standby cache, so emptying that cache moves memory to "truly free" without changing "available".</p>`}
  </div>`;
}

function renderHistory() {
  const el = $("#history");
  if (!el) return;
  if (!snap.history.length) {
    el.innerHTML = `<span class="muted">Nothing yet this session.</span>`;
    return;
  }
  el.innerHTML = snap.history
    .map((r) => {
      const { free, avail } = reportSummary(r);
      const fails = r.results.filter((x) => !x.ok).length;
      return `<div><span class="when">${when(r.at)}</span><span style="width:120px">${esc(r.trigger)}</span>
        <span>${Number.isNaN(free) ? "" : `free <span class="${free >= 0 ? "pos" : "neg"}">${signed(free)}</span>, `}available <span class="${avail >= 0 ? "pos" : "neg"}">${signed(avail)}</span></span>
        <span class="muted" style="margin-left:auto">${fails ? `${fails} step${fails > 1 ? "s" : ""} skipped` : "all steps ok"}</span></div>`;
    })
    .join("");
}

async function runOptimize() {
  if (busy) return;
  if (!snap.settings.areas.length) return toast("Pick at least one area first");
  busy = true;
  renderOverview();
  try {
    lastReport = await invoke<Report>("run_optimize", { areas: snap.settings.areas });
  } catch (e) {
    toast(String(e));
  }
  busy = false;
  renderOverview();
}

// ------------------------------------------------------------------ standby

function renderStandby() {
  const el = $("#tab-standby");
  const s = snap.settings;
  const l = stats?.lists;
  const st = snap.standby;
  const max = l ? Math.max(...l.standbyByPriority, 1) : 1;
  el.innerHTML = `
    <h1>Standby cache</h1>
    <p class="sub">Windows keeps recently used files and app data in RAM as a "standby" cache. It is handed to apps instantly when they need it, so a big standby list is normally a good thing.
    The cleaner below steps in only when the cache is large <b>and</b> truly free RAM is low, which is the case where some games stutter while Windows repurposes cache pages.</p>
    <div class="row">
      <div class="card">
        <h2>Standby list now</h2>
        ${
          l
            ? `<div class="hero" style="margin-bottom:6px"><span class="pct" style="font-size:30px">${fmt(l.standby)}</span><span class="muted">cached · ${fmt(l.zero + l.free)} truly free</span></div>
               <div class="prio">${l.standbyByPriority
                 .map((v, i) => `<span class="muted">Priority ${i}${i === 0 ? " (low)" : i >= 6 ? " (high)" : ""}</span><div class="b"><div style="width:${(v / max) * 100}%"></div></div><span style="text-align:right">${fmt(v)}</span>`)
                 .reverse()
                 .join("")}</div>
               <div class="go"><button class="btn" id="sb-low">Clear low priority</button><button class="btn warn" id="sb-all">Clear all standby</button></div>`
            : `<p class="muted">Windows only shares the standby breakdown with administrator apps.</p>`
        }
      </div>
      <div class="card">
        <h2>Automatic standby cleaner</h2>
        <div class="field"><div class="l"><div>Enabled</div><div class="d">Checks every second; never runs inside the cooldown.</div></div>${sw("standbyCleaner")}</div>
        <div class="field"><div class="l"><div>When standby is at least</div></div><span>${num("standbyMinMb", 128, 65536)} MB</span></div>
        <div class="field"><div class="l"><div>And truly free RAM is below</div></div><span>${num("standbyFreeBelowMb", 64, 65536)} MB</span></div>
        <div class="field"><div class="l"><div>Cooldown between cleans</div></div><span>${num("standbyCooldownSec", 5, 3600)} s</span></div>
        <div class="field"><div class="l"><div>Low priority pages only</div><div class="d">Gentler: keeps the cache Windows values most.</div></div>${sw("standbyLowOnly")}</div>
        <div class="note" style="margin-top:10px">${
          st.runs
            ? `Cleaned <b>${st.runs}</b> time${st.runs > 1 ? "s" : ""} this session, released <b>${fmt(st.released)}</b>. Last at ${when(st.lastAt)} (${fmt(st.lastReleased)}).`
            : snap.elevated
              ? "Has not needed to run yet this session."
              : "Needs administrator to run."
        }</div>
      </div>
    </div>`;
  bindFields(el, renderStandby);
  const clear = async (area: Area) => {
    try {
      const r = await invoke<Report>("run_optimize", { areas: [area] });
      const x = r.results[0];
      toast(x?.ok ? `Released ${fmt(trulyFree(r.after) - trulyFree(r.before))} to truly free` : `Skipped: ${x?.detail}`);
      lastReport = r;
    } catch (e) {
      toast(String(e));
    }
  };
  $("#sb-low", el)?.addEventListener("click", () => clear("standbyLow"));
  $("#sb-all", el)?.addEventListener("click", () => clear("standby"));
}

// ------------------------------------------------------------------ processes

async function loadProcs() {
  procs = await invoke<Proc[]>("get_processes");
  renderProcs();
}
function renderProcs() {
  const el = $("#tab-processes");
  const ex = new Set(snap.settings.excluded.map((e) => e.toLowerCase()));
  const max = Math.max(...procs.map((p) => p.workingSet), 1);
  el.innerHTML = `
    <h1>Processes</h1>
    <p class="sub">The 60 apps using the most RAM. Excluded apps are never trimmed. Trim releases one app's idle pages only.</p>
    <div class="card" style="padding:6px 10px">
      <table>
        <thead><tr><th>Name</th><th class="num">PID</th><th style="width:30%">Working set</th><th class="num">Private</th><th class="num"></th></tr></thead>
        <tbody>${procs
          .map((p) => {
            const isEx = ex.has(p.name.toLowerCase());
            return `<tr class="${isEx ? "ex" : ""}"><td>${esc(p.name)}</td><td class="num muted">${p.pid}</td>
              <td>${fmt(p.workingSet)}<div class="mini"><div style="width:${(p.workingSet / max) * 100}%"></div></div></td>
              <td class="num">${fmt(p.private)}</td>
              <td class="num" style="white-space:nowrap"><button class="btn small" data-trim="${p.pid}" ${isEx ? "disabled" : ""}>Trim</button>
                <button class="btn small" data-ex="${esc(p.name)}">${isEx ? "Include" : "Exclude"}</button></td></tr>`;
          })
          .join("")}</tbody>
      </table>
    </div>`;
  el.querySelectorAll<HTMLButtonElement>("[data-trim]").forEach((b) =>
    b.addEventListener("click", async () => {
      const p = procs.find((x) => x.pid === +b.dataset.trim!);
      try {
        await invoke("trim_process", { pid: +b.dataset.trim! });
        toast(`Trimmed ${p?.name}`);
        setTimeout(loadProcs, 400);
      } catch (e) {
        toast(String(e));
      }
    }),
  );
  el.querySelectorAll<HTMLButtonElement>("[data-ex]").forEach((b) =>
    b.addEventListener("click", () => {
      const n = b.dataset.ex!.toLowerCase();
      const list = snap.settings.excluded.filter((e) => e.toLowerCase() !== n);
      if (list.length === snap.settings.excluded.length) list.push(n);
      snap.settings.excluded = list;
      saveSettings();
      renderProcs();
    }),
  );
}

// ------------------------------------------------------------------ settings

function sw(key: keyof Settings) {
  return `<label class="switch"><input type="checkbox" data-key="${key}" ${snap.settings[key] ? "checked" : ""}><span></span></label>`;
}
function num(key: keyof Settings, min: number, max: number) {
  return `<input type="number" data-key="${key}" min="${min}" max="${max}" value="${snap.settings[key]}">`;
}
function bindFields(root: HTMLElement, rerender?: () => void) {
  root.querySelectorAll<HTMLInputElement>("[data-key]").forEach((i) =>
    i.addEventListener("change", () => {
      const k = i.dataset.key as keyof Settings;
      let v: unknown;
      if (i.type === "checkbox") v = i.checked;
      else if (i.type === "number") {
        const n = Math.round(+i.value);
        v = Math.min(+i.max, Math.max(+i.min, Number.isFinite(n) ? n : +i.min));
        i.value = String(v);
      } else v = i.value.trim();
      (snap.settings as any)[k] = v;
      saveSettings(true);
      rerender?.();
    }),
  );
}

async function renderSettings() {
  const el = $("#tab-settings");
  const startup = await invoke<boolean>("get_startup").catch(() => false);
  el.innerHTML = `
    <h1>Settings</h1>
    <p class="sub">Saved to %APPDATA%\\MemPilot\\settings.json. MemPilot has no auto-update and sends nothing over the network.</p>
    <div class="row">
      <div class="card">
        <h2>General</h2>
        <div class="field"><div class="l"><div>Start with Windows</div><div class="d">Elevated logon task, no UAC prompt at boot. Needs administrator to change.</div></div>
          <label class="switch"><input type="checkbox" id="startup" ${startup ? "checked" : ""}><span></span></label></div>
        <div class="field"><div class="l">Start minimized to tray</div>${sw("startMinimized")}</div>
        <div class="field"><div class="l">Close button hides to tray</div>${sw("closeToTray")}</div>
        <div class="field"><div class="l">Always on top</div>${sw("alwaysOnTop")}</div>
        <div class="field"><div class="l"><div>Run at low priority</div><div class="d">MemPilot yields CPU to your apps.</div></div>${sw("lowPriority")}</div>
        <div class="field"><div class="l"><div>Never trim the app you are using</div><div class="d">Protects your game or editor during hotkey, tray and automatic runs.</div></div>${sw("skipForeground")}</div>
      </div>
      <div class="card">
        <h2>Tray icon</h2>
        <div class="field"><div class="l">Show memory use % in the icon</div>${sw("trayShowUsage")}</div>
        <div class="field"><div class="l">Middle click optimizes</div>${sw("middleClickOptimize")}</div>
        <div class="field"><div class="l">Warning colour from</div><span>${num("warnPct", 10, 100)} %</span></div>
        <div class="field"><div class="l">Danger colour from</div><span>${num("dangerPct", 10, 100)} %</span></div>
        <h2 style="margin-top:16px">Hotkey</h2>
        <div class="field"><div class="l">Optimize with a hotkey</div>${sw("hotkeyEnabled")}</div>
        <div class="field"><div class="l"><div>Shortcut</div><div class="d">For example Ctrl+Shift+M or Alt+F9.</div></div><input type="text" data-key="hotkey" value="${esc(snap.settings.hotkey)}"></div>
      </div>
    </div>
    <div class="row">
      <div class="card">
        <h2>Automatic optimization</h2>
        <div class="field"><div class="l"><div>When available RAM drops below</div><div class="d">0 turns it off.</div></div><span>${num("autoBelowPct", 0, 90)} %</span></div>
        <div class="field"><div class="l"><div>Every</div><div class="d">0 turns it off.</div></div><span>${num("autoEveryHours", 0, 48)} h</span></div>
        <div class="field"><div class="l"><div>Minimum gap between automatic runs</div><div class="d">Stops a busy PC from being cleaned in a loop.</div></div><span>${num("autoCooldownMin", 1, 240)} min</span></div>
        <p class="muted small" style="margin:8px 0 0">Automatic runs use the areas ticked on the Overview page.</p>
      </div>
      <div class="card">
        <h2>Never trim these apps</h2>
        <div class="chips">${snap.settings.excluded.map((e) => `<span class="chip">${esc(e)}<button data-rm="${esc(e)}" title="Remove">✕</button></span>`).join("") || `<span class="muted">None</span>`}</div>
        <div class="addrow"><input id="exin" placeholder="game.exe" spellcheck="false"><button class="btn" id="exadd">Add</button></div>
        <p class="muted small" style="margin:8px 0 0">audiodg.exe and dwm.exe are on the list by default: trimming them can crackle audio or hitch the desktop.</p>
      </div>
    </div>`;
  bindFields(el);
  $("#startup", el).addEventListener("change", async (e) => {
    const box = e.target as HTMLInputElement;
    try {
      await invoke("set_startup", { enabled: box.checked });
      toast(box.checked ? "MemPilot will start with Windows" : "Removed from startup");
    } catch (err) {
      box.checked = !box.checked;
      toast(snap.elevated ? String(err) : "Restart as administrator to change startup");
    }
  });
  el.querySelectorAll<HTMLButtonElement>("[data-rm]").forEach((b) =>
    b.addEventListener("click", () => {
      snap.settings.excluded = snap.settings.excluded.filter((x) => x !== b.dataset.rm);
      saveSettings(true);
      renderSettings();
    }),
  );
  const add = () => {
    const v = $<HTMLInputElement>("#exin", el).value.trim().toLowerCase();
    if (!v) return;
    const name = v.endsWith(".exe") ? v : v + ".exe";
    if (!snap.settings.excluded.includes(name)) snap.settings.excluded.push(name);
    saveSettings(true);
    renderSettings();
  };
  $("#exadd", el).addEventListener("click", add);
  $("#exin", el).addEventListener("keydown", (e) => e.key === "Enter" && add());
}

// ------------------------------------------------------------------ shell

function renderAdmin() {
  $("#admin").innerHTML = snap.elevated
    ? `<div class="admin yes">Administrator: all steps available</div>`
    : `<div class="admin no">Limited mode: most steps need administrator<button class="btn small" id="elev">Restart as administrator</button></div>`;
  $("#elev")?.addEventListener("click", () => invoke("restart_as_admin").catch((e) => toast(String(e))));
}

let activeTab = "overview";
function showTab(t: string) {
  activeTab = t;
  document.querySelectorAll("nav button").forEach((b) => b.classList.toggle("on", (b as HTMLElement).dataset.tab === t));
  document.querySelectorAll(".tab").forEach((s) => s.classList.toggle("on", s.id === "tab-" + t));
  if (t === "overview") renderOverview();
  if (t === "standby") renderStandby();
  if (t === "processes") loadProcs();
  if (t === "cleaner") renderCleaner();
  if (t === "dupes") renderDupes();
  if (t === "health") renderHealth();
  if (t === "settings") renderSettings();
}

async function init() {
  snap = await invoke<Snapshot>("get_snapshot");
  stats = snap.stats;
  lastReport = snap.history[0] ?? null;
  $("#ver").textContent = `v${snap.version} · no telemetry`;
  renderAdmin();
  document.querySelectorAll<HTMLButtonElement>("nav button").forEach((b) => b.addEventListener("click", () => showTab(b.dataset.tab!)));
  showTab("overview");

  let tick = 0;
  listen<Stats>("stats", (s) => {
    stats = s;
    spark.push({ used: s.usedPct, standby: s.lists ? (s.lists.standby / s.total) * 100 : 0 });
    if (spark.length > 120) spark.shift();
    if (activeTab === "overview") renderMem();
    if (activeTab === "standby" && ++tick % 2 === 0 && !document.activeElement?.matches("input")) renderStandby();
    if (activeTab === "processes" && tick % 3 === 0) loadProcs();
    if (activeTab !== "standby") tick++;
  });
  listen<Report>("optimized", (r) => {
    snap.history = [r, ...snap.history.filter((h) => h.at !== r.at)].slice(0, 50);
    lastReport = r;
    if (activeTab === "overview") { renderResult(); renderHistory(); }
    if (r.trigger !== "manual") {
      const f = trulyFree(r.after) - trulyFree(r.before);
      toast(`Optimized (${r.trigger}): ${Number.isNaN(f) ? signed(r.after.available - r.before.available) + " available" : signed(f) + " free"}`);
    }
  });
  listen<StandbyStatus>("standby", (s) => {
    snap.standby = s;
    if (activeTab === "standby") renderStandby();
  });
  addEventListener("resize", drawSpark);
}

init();
