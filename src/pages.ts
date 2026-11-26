import { $, esc, fmt, toast, invoke, listen } from "./lib";

// ================================================================== junk cleaner

interface Cat { id: string; group: string; name: string; desc: string; defaultOn: boolean; needsAdmin: boolean; bytes: number; files: number; blocked: string | null }
interface CleanResult { id: string; name: string; freed: number; deleted: number; skipped: number; note: string | null }

let cats: Cat[] | null = null;
let picked = new Set<string>();
let cleanResults: CleanResult[] | null = null;
let cleaning = false;
let dismOut = "";
let dismBusy = false;

async function scanCleaner() {
  cats = null;
  drawCleaner();
  const first = !picked.size;
  cats = await invoke<Cat[]>("cleaner_scan");
  if (first) picked = new Set(cats.filter((c) => c.defaultOn).map((c) => c.id));
  drawCleaner();
}

export function renderCleaner() {
  if (!cats) scanCleaner();
  else drawCleaner();
}

function drawCleaner() {
  const el = $("#tab-cleaner");
  const total = cats?.filter((c) => picked.has(c.id) && !c.blocked).reduce((a, c) => a + c.bytes, 0) ?? 0;
  const groups = [...new Set((cats ?? []).map((c) => c.group))];
  el.innerHTML = `
    <h1>Junk cleaner</h1>
    <p class="sub">Temporary files, caches and package downloads that apps recreate when needed. Files changed in the last day, files in use, and caches of open browsers or apps are always left alone. Logins, history, passwords and your own files are never touched.</p>
    ${
      !cats
        ? `<div class="card muted">Measuring junk on your drives…</div>`
        : groups
            .map(
              (g) => `<div class="card"><h2>${esc(g)}</h2>${cats!
                .filter((c) => c.group === g)
                .map(
                  (c) => `<label class="area clean-row${c.blocked ? " dis" : ""}">
                    <input type="checkbox" data-cat="${c.id}" ${picked.has(c.id) ? "checked" : ""} ${c.blocked ? "disabled" : ""}>
                    <div style="flex:1"><div class="t">${esc(c.name)}${c.needsAdmin ? `<span class="tag cost">admin</span>` : ""}${c.defaultOn ? "" : `<span class="tag muted-tag">optional</span>`}</div>
                    <div class="d">${esc(c.blocked ?? c.desc)}</div></div>
                    <div class="sz">${c.bytes ? fmt(c.bytes) : `<span class="muted">empty</span>`}<div class="d">${c.files ? `${c.files.toLocaleString()} files` : ""}</div></div>
                  </label>`,
                )
                .join("")}</div>`,
            )
            .join("")
    }
    ${
      cats
        ? `<div class="card sticky-go"><div class="go" style="margin:0">
            <button class="btn primary" id="clean-go" ${cleaning || !total ? "disabled" : ""}>${cleaning ? "Cleaning…" : `Clean ${fmt(total)}`}</button>
            <button class="btn" id="clean-rescan" ${cleaning ? "disabled" : ""}>Scan again</button>
            <span class="muted small">Deleted files skip the Recycle Bin. Everything listed here is rebuilt automatically when needed.</span></div>
            ${
              cleanResults
                ? `<div class="result"><div class="big">Freed <span class="pos">${fmt(cleanResults.reduce((a, r) => a + r.freed, 0))}</span></div>
                   <ul>${cleanResults.map((r) => `<li><span class="${r.freed || !r.note ? "ok" : "no"}">${r.freed || !r.note ? "✓" : "–"}</span><span>${esc(r.name)}</span><span class="muted">${fmt(r.freed)}${r.note ? " · " + esc(r.note) : ""}</span></li>`).join("")}</ul></div>`
                : ""
            }</div>`
        : ""
    }
    <div class="card">
      <h2>Old Windows update packages</h2>
      <p class="muted small" style="margin:0 0 10px">Windows keeps replaced versions of system files after updates. This runs Windows' own clean-up (DISM StartComponentCleanup), the same job Windows schedules itself. Installed updates can still be uninstalled afterwards. Needs administrator; can take several minutes.</p>
      <div class="go" style="margin:0"><button class="btn" id="dism-an" ${dismBusy ? "disabled" : ""}>Check size</button><button class="btn" id="dism-go" ${dismBusy ? "disabled" : ""}>Clean up</button>
      <span class="muted small">${dismBusy ? "Working, keep MemPilot open…" : esc(dismOut)}</span></div>
    </div>`;
  el.querySelectorAll<HTMLInputElement>("[data-cat]").forEach((c) =>
    c.addEventListener("change", () => {
      c.checked ? picked.add(c.dataset.cat!) : picked.delete(c.dataset.cat!);
      drawCleaner();
    }),
  );
  $("#clean-rescan", el)?.addEventListener("click", () => { cleanResults = null; scanCleaner(); });
  $("#clean-go", el)?.addEventListener("click", async () => {
    cleaning = true;
    drawCleaner();
    cleanResults = await invoke<CleanResult[]>("cleaner_clean", { ids: [...picked] });
    cleaning = false;
    cats = await invoke<Cat[]>("cleaner_scan");
    drawCleaner();
  });
  const dism = async (analyze: boolean) => {
    dismBusy = true;
    drawCleaner();
    try {
      dismOut = await invoke<string>("component_store", { analyze });
    } catch (e) {
      dismOut = String(e);
    }
    dismBusy = false;
    if ($("#tab-cleaner").classList.contains("on")) drawCleaner();
  };
  $("#dism-an", el)?.addEventListener("click", () => dism(true));
  $("#dism-go", el)?.addEventListener("click", () => dism(false));
}

// ================================================================== duplicates

interface DupFile { path: string; modified: number }
interface DupGroup { size: number; files: DupFile[] }
interface Progress { phase: string; done: number; total: number }

let roots: string[] | null = null;
let filter = "all";
let minBytes = 1024 * 1024;
let groups: DupGroup[] | null = null;
let scanning = false;
let progress: Progress | null = null;
// per group: index of the copy to keep, and the set of indexes to remove
let keep: number[] = [];
let remove: Set<number>[] = [];
const SHOW = 150;

listen<Progress>("dupes-progress", (p) => {
  progress = p;
  const bar = document.getElementById("dupbar");
  if (bar) bar.innerHTML = progressHtml();
});

function progressHtml() {
  if (!progress) return "Starting…";
  const pct = progress.total ? Math.round((progress.done / progress.total) * 100) : 0;
  return `<div class="muted small">${esc(progress.phase)}${progress.total ? ` · ${progress.done.toLocaleString()} of ${progress.total.toLocaleString()} same-size files` : ""}</div>
    <div class="mini" style="height:6px;margin-top:6px"><div style="width:${progress.total ? pct : 5}%;background:var(--accent)"></div></div>`;
}

export async function renderDupes() {
  if (!roots) roots = await invoke<string[]>("dupes_default_roots");
  drawDupes();
}

const base = (p: string) => p.split("\\").pop() ?? p;
const dir = (p: string) => p.slice(0, p.length - base(p).length);

function selectedBytes() {
  let n = 0, b = 0;
  (groups ?? []).forEach((g, i) => { n += remove[i]?.size ?? 0; b += (remove[i]?.size ?? 0) * g.size; });
  return { n, b };
}

function drawDupes() {
  const el = $("#tab-dupes");
  const sel = selectedBytes();
  const waste = (groups ?? []).reduce((a, g) => a + g.size * (g.files.length - 1), 0);
  el.innerHTML = `
    <h1>Duplicate files</h1>
    <p class="sub">Finds files with exactly the same content, even under different names. Nothing is removed until you press the button, each copy is re-checked byte for byte first, and removed copies go to the Recycle Bin so you can restore them.</p>
    <div class="card">
      <h2>Where to look</h2>
      <div class="chips">${roots!.map((r, i) => `<span class="chip">${esc(r)}<button data-rmroot="${i}" title="Remove">✕</button></span>`).join("") || `<span class="muted">No folders</span>`}</div>
      <div class="go" style="margin-top:10px"><button class="btn" id="addroot">Add folder…</button>
        <span class="muted small">App folders, Windows, node_modules, .git and hidden or cloud-only files are always skipped.</span></div>
      <div class="go">
        <div class="presets" style="margin:0">${["all", "images", "videos", "music", "documents", "archives"].map((f) => `<button data-filter="${f}" class="${filter === f ? "on" : ""}">${f[0].toUpperCase() + f.slice(1)}</button>`).join("")}</div>
        <label class="muted small">Ignore files under <select id="minsize">${[[100 * 1024, "100 KB"], [1024 ** 2, "1 MB"], [10 * 1024 ** 2, "10 MB"], [100 * 1024 ** 2, "100 MB"]].map(([v, l]) => `<option value="${v}" ${v === minBytes ? "selected" : ""}>${l}</option>`).join("")}</select></label>
      </div>
      <div class="go">
        ${scanning ? `<button class="btn" id="dupcancel">Stop</button>` : `<button class="btn primary" id="dupscan" ${roots!.length ? "" : "disabled"}>Find duplicates</button>`}
        <div id="dupbar" style="flex:1">${scanning ? progressHtml() : ""}</div>
      </div>
    </div>
    ${
      groups
        ? groups.length
          ? `<div class="card sticky-go"><div class="go" style="margin:0">
              <button class="btn primary" id="duprm" ${sel.n ? "" : "disabled"}>Move ${sel.n} cop${sel.n === 1 ? "y" : "ies"} to Recycle Bin (${fmt(sel.b)})</button>
              <button class="btn" id="dupauto">Select all but the oldest</button><button class="btn" id="dupnone">Select none</button>
              <span class="muted small">${groups.length.toLocaleString()} sets · ${fmt(waste)} taken by extra copies</span></div></div>
             ${groups.slice(0, SHOW).map((g, gi) => `<div class="card dupgroup"><div class="muted small" style="margin-bottom:6px">${g.files.length} identical files · ${fmt(g.size)} each</div>
               ${g.files.map((f, fi) => `<div class="duprow ${keep[gi] === fi ? "keep" : ""}">
                 <input type="checkbox" data-g="${gi}" data-f="${fi}" ${remove[gi].has(fi) ? "checked" : ""} ${keep[gi] === fi ? "disabled" : ""} title="Remove this copy">
                 <div class="dp"><div class="fn">${esc(base(f.path))}</div><div class="d">${esc(dir(f.path))}</div></div>
                 <span class="muted small">${new Date(f.modified).toLocaleDateString()}</span>
                 ${keep[gi] === fi ? `<span class="tag safe">kept</span>` : `<button class="btn small" data-keep="${gi}:${fi}">Keep this</button>`}
                 <button class="btn small" data-reveal="${esc(f.path)}">Show</button></div>`).join("")}</div>`).join("")}
             ${groups.length > SHOW ? `<p class="muted small">Showing the ${SHOW} biggest sets. Remove these, then scan again for the rest.</p>` : ""}`
          : `<div class="card">No duplicates found in these folders.</div>`
        : ""
    }`;
  el.querySelectorAll<HTMLButtonElement>("[data-rmroot]").forEach((b) => b.addEventListener("click", () => { roots!.splice(+b.dataset.rmroot!, 1); drawDupes(); }));
  $("#addroot", el).addEventListener("click", async () => {
    const p = await invoke<string | null>("pick_folder");
    if (p && !roots!.includes(p)) roots!.push(p);
    drawDupes();
  });
  el.querySelectorAll<HTMLButtonElement>("[data-filter]").forEach((b) => b.addEventListener("click", () => { filter = b.dataset.filter!; drawDupes(); }));
  $<HTMLSelectElement>("#minsize", el).addEventListener("change", (e) => { minBytes = +(e.target as HTMLSelectElement).value; });
  $("#dupscan", el)?.addEventListener("click", async () => {
    scanning = true; progress = null; groups = null;
    drawDupes();
    const g = await invoke<DupGroup[]>("dupes_scan", { roots, filter, minBytes });
    scanning = false;
    groups = g;
    keep = g.map(() => 0);
    remove = g.map((x) => new Set(x.files.map((_, i) => i).filter((i) => i !== 0)));
    drawDupes();
  });
  $("#dupcancel", el)?.addEventListener("click", () => invoke("dupes_cancel"));
  el.querySelectorAll<HTMLInputElement>("[data-g]").forEach((c) =>
    c.addEventListener("change", () => {
      const g = +c.dataset.g!, f = +c.dataset.f!;
      c.checked ? remove[g].add(f) : remove[g].delete(f);
      drawDupes();
    }),
  );
  el.querySelectorAll<HTMLButtonElement>("[data-keep]").forEach((b) =>
    b.addEventListener("click", () => {
      const [g, f] = b.dataset.keep!.split(":").map(Number);
      keep[g] = f;
      remove[g].delete(f);
      drawDupes();
    }),
  );
  el.querySelectorAll<HTMLButtonElement>("[data-reveal]").forEach((b) => b.addEventListener("click", () => invoke("reveal", { path: b.dataset.reveal }).catch((e) => toast(String(e)))));
  $("#dupauto", el)?.addEventListener("click", () => {
    groups!.forEach((g, i) => { keep[i] = 0; remove[i] = new Set(g.files.map((_, j) => j).filter((j) => j !== 0)); });
    drawDupes();
  });
  $("#dupnone", el)?.addEventListener("click", () => { remove = remove.map(() => new Set()); drawDupes(); });
  $("#duprm", el)?.addEventListener("click", async () => {
    const requests = groups!
      .map((g, i) => ({ keep: g.files[keep[i]].path, remove: [...remove[i]].filter((j) => j !== keep[i]).map((j) => g.files[j].path) }))
      .filter((r) => r.remove.length);
    const r = await invoke<{ recycled: number; bytes: number; refused: string[] }>("dupes_remove", { requests });
    toast(`Moved ${r.recycled} file${r.recycled === 1 ? "" : "s"} (${fmt(r.bytes)}) to the Recycle Bin${r.refused.length ? `, ${r.refused.length} left alone (changed or in use)` : ""}`);
    const gone = new Set(requests.flatMap((q) => q.remove).filter((p) => !r.refused.includes(p)));
    const next: DupGroup[] = [], nk: number[] = [], nr: Set<number>[] = [];
    groups!.forEach((g, i) => {
      const files = g.files.filter((f) => !gone.has(f.path));
      if (files.length > 1) {
        next.push({ size: g.size, files });
        nk.push(Math.max(0, files.findIndex((f) => f.path === g.files[keep[i]].path)));
        nr.push(new Set());
      }
    });
    groups = next; keep = nk; remove = nr;
    drawDupes();
  });
}

// ================================================================== laptop health

interface Power { onAc: boolean; hasBattery: boolean; percent: number | null; saver: boolean }
interface Nvme { drive: number; criticalWarning: number; tempC: number; availableSpare: number; spareThreshold: number; percentUsed: number; bytesRead: number; bytesWritten: number; powerCycles: number; powerOnHours: number; unsafeShutdowns: number; mediaErrors: number; warningTempMinutes: number; criticalTempMinutes: number }
interface Volume { root: string; label: string; fs: string; kind: string; total: number; free: number }
interface HealthReport { power: Power; report: any; nvme: Nvme[]; volumes: Volume[] }
interface GpuApp { pid: number; name: string; path: string; dedicated: number; util: number; preference: number }
interface Gpu { name: string; vendor: string; vram: number; shared: number; util: number; dedicatedUsed: number; discrete: boolean; apps: GpuApp[] }

let hr: HealthReport | null = null;
let gpus: Gpu[] | null = null;
let smi: Record<string, string> | null = null;
let smiErr = "";
let gpuTimer = 0;

export async function renderHealth() {
  drawHealth();
  const [h, g] = await Promise.all([invoke<HealthReport>("health_report"), invoke<Gpu[]>("gpu_status")]);
  hr = h; gpus = g;
  if (hr.power.onAc && gpus.some((x) => x.vendor === "NVIDIA")) await readSmi(false);
  drawHealth();
  clearInterval(gpuTimer);
  gpuTimer = window.setInterval(async () => {
    if (!$("#tab-health").classList.contains("on")) return clearInterval(gpuTimer);
    gpus = await invoke<Gpu[]>("gpu_status");
    if (hr) hr.power = await invoke<Power>("power_status");
    if (hr?.power.onAc && smi) await readSmi(false);
    drawHealth();
  }, 5000);
}

async function readSmi(redraw = true) {
  try { smi = await invoke<Record<string, string>>("nvidia_sensors"); smiErr = ""; } catch (e) { smiErr = String(e); }
  if (redraw) drawHealth();
}

type Level = "good" | "warn" | "bad" | "info";
const dot = (l: Level) => `<span class="lvl ${l}"></span>`;
const stat = (k: string, v: string, l?: Level) => `<div class="tile"><div class="k">${k}</div><div class="v">${l ? dot(l) : ""}${v}</div></div>`;

function drawHealth() {
  const el = $("#tab-health");
  if (!hr) {
    el.innerHTML = `<h1>Laptop health</h1><p class="sub">Reading battery, SSD, GPU and hardware logs…</p>`;
    return;
  }
  const r = hr.report ?? {};
  const bat = (r.batteries ?? [])[0];
  const st = r.status;
  const tips: { l: Level; t: string }[] = [];

  // ---------- battery
  let batHtml = `<p class="muted">No battery found.</p>`;
  if (bat && bat.design > 0) {
    const health = (bat.full / bat.design) * 100;
    const hl: Level = health >= 80 ? "good" : health >= 60 ? "warn" : "bad";
    const pct = hr.power.percent;
    const held = hr.power.onAc && st && !st.charging && pct !== null && pct < 98;
    batHtml = `<div class="tiles">
        ${stat("Battery health", `${health.toFixed(0)}%`, hl)}
        ${stat("Holds now / when new", `${(bat.full / 1000).toFixed(1)} / ${(bat.design / 1000).toFixed(1)} Wh`)}
        ${stat("Charge cycles", bat.cycles > 0 ? String(bat.cycles) : `<span class="muted small">not reported by battery</span>`)}
        ${stat("Charge now", pct !== null ? `${pct}%` : "—")}
        ${stat("Power", hr.power.onAc ? (st?.charging ? `Charging${st.chargeRate ? ` at ${(st.chargeRate / 1000).toFixed(0)} W` : ""}` : "Plugged in, not charging") : `On battery${st?.dischargeRate ? `, using ${(st.dischargeRate / 1000).toFixed(1)} W` : ""}`)}
        ${stat("Power mode", esc(r.plan ?? "—"))}
      </div>
      ${held ? `<div class="note" style="margin-top:10px">${dot("good")}Plugged in and holding at ${pct}% instead of charging to 100%: a charge limit (Armoury Crate Battery Care) looks active. That is the single best thing for battery lifespan.</div>` : ""}`;
    if (!held && hr.power.onAc && pct !== null && pct >= 98)
      tips.push({ l: "warn", t: "Battery sits at 100% while plugged in. Set a charge limit of 80% in Armoury Crate → Battery Care Mode (Balanced). Lithium cells age fastest when kept full and warm." });
    if (hl !== "good") tips.push({ l: hl, t: `Battery holds ${health.toFixed(0)}% of its original capacity. Below about 80% runtime drops noticeably; a replacement restores it.` });
  }

  // ---------- storage
  const disks: any[] = r.disks ?? [];
  const nv = hr.nvme;
  const ssdHtml =
    (nv.length
      ? nv
          .map((n) => {
            const d = disks.find((x) => String(x.id) === String(n.drive));
            const life = 100 - n.percentUsed;
            const ll: Level = n.percentUsed < 70 ? "good" : n.percentUsed < 90 ? "warn" : "bad";
            const tl: Level = n.tempC < 60 ? "good" : n.tempC < 70 ? "warn" : "bad";
            if (n.criticalWarning) tips.push({ l: "bad", t: `The SSD reports a critical warning (code ${n.criticalWarning}). Back up your files now.` });
            if (n.mediaErrors) tips.push({ l: "bad", t: `The SSD has logged ${n.mediaErrors} media errors. Keep backups current.` });
            if (ll !== "good") tips.push({ l: ll, t: `The SSD has used ${n.percentUsed}% of its rated write endurance.` });
            return `<div class="muted small" style="margin-bottom:8px">${esc(d?.name ?? `Disk ${n.drive}`)}</div><div class="tiles">
              ${stat("SSD life left", `${life}%`, ll)}
              ${stat("Data written, ever", `${(n.bytesWritten / 1e12).toFixed(1)} TB`)}
              ${stat("Temperature", `${n.tempC} °C`, tl)}
              ${stat("Powered on", `${n.powerOnHours.toLocaleString()} h`)}
              ${stat("Spare blocks", `${n.availableSpare}%`, n.availableSpare > n.spareThreshold ? "good" : "bad")}
              ${stat("Unsafe shutdowns", n.unsafeShutdowns.toLocaleString(), n.unsafeShutdowns > 100 ? "warn" : "info")}
            </div>`;
          })
          .join("")
      : disks
          .map((d) => `<div class="muted small" style="margin-bottom:8px">${esc(d.name ?? "Disk")}</div><div class="tiles">
            ${stat("Windows health", esc(d.health ?? "—"), d.health === "Healthy" ? "good" : "warn")}
            ${stat("Wear", d.wear != null ? `${d.wear}% used` : `<span class="muted small">needs administrator</span>`)}
            ${stat("Temperature", d.temp ? `${d.temp} °C` : `<span class="muted small">needs administrator</span>`)}</div>`)
          .join("")) +
    `<table style="margin-top:12px"><tbody>${hr.volumes
      .map((v) => {
        const p = v.total ? ((v.total - v.free) / v.total) * 100 : 0;
        if (v.kind === "Fixed" && p > 85) tips.push({ l: "warn", t: `${v.root} is ${p.toFixed(0)}% full. SSDs slow down and wear faster when nearly full; keep 15–20% free (the Junk cleaner and Duplicates pages help).` });
        const col = p >= 90 ? "var(--bad)" : p >= 85 ? "var(--warn)" : "var(--inuse)";
        return `<tr><td style="width:70px">${esc(v.root)}</td><td>${fmt(v.total - v.free)} of ${fmt(v.total)} used <span class="muted">(${fmt(v.free)} free)</span><div class="mini"><div style="width:${p}%;background:${col}"></div></div></td></tr>`;
      })
      .join("")}</tbody></table>
    <div class="note" style="margin-top:10px">${dot(r.trim === false ? "bad" : "good")}TRIM is ${r.trim === false ? "<b>off</b>: Windows is not telling the SSD which blocks are free, which costs speed and lifespan" : "on, so the SSD can clean up freed blocks efficiently"}.</div>`;
  if (r.trim === false) tips.push({ l: "bad", t: "Turn TRIM back on (admin terminal: fsutil behavior set DisableDeleteNotify 0)." });

  // ---------- GPU
  const gpuHtml = (gpus ?? [])
    .map((g) => {
      const dgpu = g.vendor === "NVIDIA";
      const awake = dgpu && g.apps.length > 0;
      if (dgpu && awake && hr && !hr.power.onAc)
        tips.push({ l: "warn", t: `On battery with the ${g.name} awake (${g.apps.length} app${g.apps.length > 1 ? "s" : ""} using it). Switch those apps to the power-saving GPU below so it can sleep.` });
      const temp = smi && dgpu ? +smi["temperature.gpu"] : NaN;
      if (temp >= 87) tips.push({ l: "warn", t: `The NVIDIA GPU is at ${temp} °C. Raise the laptop's back for airflow and clean the vents; heat is what ages a GPU.` });
      return `<div class="gpu"><div class="gh"><b>${esc(g.name)}</b><span class="muted small">${fmt(g.vram)} dedicated · busy ${g.util.toFixed(0)}%</span>
          ${dgpu ? `<span class="tag ${awake ? "cost" : "safe"}">${awake ? "awake" : "sleeping"}</span>` : ""}</div>
        ${
          dgpu && smi
            ? `<div class="tiles" style="margin-top:8px">${stat("Temperature", `${smi["temperature.gpu"]} °C`, temp < 80 ? "good" : temp < 87 ? "warn" : "bad")}${stat("Power draw", `${smi["power.draw"]} W`)}${stat("Clock", `${smi["clocks.gr"]} MHz`)}${stat("VRAM", `${smi["memory.used"]} / ${smi["memory.total"]} MB`)}${stat("Power state", esc(smi["pstate"]) + `<span class="muted small"> (P8 = idle)</span>`)}${stat("Driver", esc(smi["driver_version"]))}</div>`
            : dgpu
              ? `<div class="go" style="margin-top:8px"><button class="btn small" id="smi">Read temperature and power</button><span class="muted small">${smiErr ? esc(smiErr) : "Reading the sensors wakes the GPU, so on battery this only runs when you ask."}</span></div>`
              : ""
        }
        ${
          g.apps.length
            ? `<table style="margin-top:8px"><thead><tr><th>App using this GPU</th><th class="num">GPU memory</th><th class="num">Busy</th>${dgpu ? `<th class="num">Battery</th>` : ""}</tr></thead><tbody>${g.apps
                .slice(0, 12)
                .map((a) => `<tr><td>${esc(a.name)}</td><td class="num">${fmt(a.dedicated)}</td><td class="num">${a.util.toFixed(0)}%</td>${
                  dgpu ? `<td class="num">${a.path ? (a.preference === 1 ? `<button class="btn small" data-gpref="${esc(a.path)}" data-v="0">Undo power saving</button>` : `<button class="btn small" data-gpref="${esc(a.path)}" data-v="1">Use power-saving GPU</button>`) : `<span class="muted small">system</span>`}</td>` : ""
                }</tr>`)
                .join("")}</tbody></table>`
            : `<p class="muted small" style="margin:6px 0 0">No app is using this GPU right now.</p>`
        }</div>`;
    })
    .join("");

  // ---------- system
  const whea = r.whea30d ?? 0;
  const crashes = r.unexpectedShutdowns30d ?? 0;
  if (whea > 0) tips.push({ l: "warn", t: `Windows logged ${whea} hardware error${whea > 1 ? "s" : ""} (WHEA) in 30 days. A few corrected errors are harmless; many can point to unstable RAM, CPU or a driver.` });
  if (crashes > 2) tips.push({ l: "warn", t: `${crashes} unexpected shutdowns in 30 days (power loss or hard resets). These stress the SSD; shut down from the Start menu when you can.` });
  if (!tips.length) tips.push({ l: "good", t: "Nothing needs attention right now." });

  el.innerHTML = `
    <h1>Laptop health</h1>
    <p class="sub">What actually wears out in a laptop and what slows that down: battery chemistry, SSD writes, and heat. Read only, except the per-app GPU switch, which is the same setting as Windows Settings → Display → Graphics.</p>
    <div class="card"><h2>What to do</h2><div class="tips">${tips.map((t) => `<div>${dot(t.l)}<span>${esc(t.t)}</span></div>`).join("")}</div></div>
    <div class="card"><h2>Battery</h2>${batHtml}</div>
    <div class="card"><h2>Storage</h2>${ssdHtml}</div>
    <div class="card"><h2>Graphics</h2>${gpuHtml || `<p class="muted">No GPU found.</p>`}
      <p class="muted small" style="margin:10px 0 0">An app switched to the power-saving GPU uses Intel graphics after you restart it. Games should stay on the NVIDIA GPU.</p></div>
    <div class="row">
      <div class="card"><h2>Hardware errors (30 days)</h2><div class="tiles" style="grid-template-columns:1fr 1fr;margin:0">${stat("Hardware errors (WHEA)", String(whea), whea ? "warn" : "good")}${stat("Unexpected shutdowns", String(crashes), crashes > 2 ? "warn" : "good")}</div></div>
      <div class="card"><h2>RAM</h2><p class="muted small" style="margin:0">Memory chips do not wear out from use, so there is no RAM lifespan to extend. The memory tools in MemPilot are for speed when RAM runs low. The hardware error count on the left is where failing RAM would show up.</p></div>
    </div>
    <div class="go"><button class="btn" id="hrefresh">Refresh</button></div>`;

  $("#smi", el)?.addEventListener("click", () => readSmi());
  $("#hrefresh", el).addEventListener("click", () => { hr = null; renderHealth(); });
  el.querySelectorAll<HTMLButtonElement>("[data-gpref]").forEach((b) =>
    b.addEventListener("click", async () => {
      try {
        await invoke("set_gpu_preference", { path: b.dataset.gpref, preference: +b.dataset.v! });
        toast(+b.dataset.v! ? "Saved. Restart that app for it to move to the power-saving GPU." : "Back to Windows' default choice.");
        gpus = await invoke<Gpu[]>("gpu_status");
        drawHealth();
      } catch (e) {
        toast(String(e));
      }
    }),
  );
}
