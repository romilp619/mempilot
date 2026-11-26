// Drives the real exe over WebView2 CDP. Read-only on the system except: trims MemPilot's OWN
// working set, and calls two optimize steps that a non-elevated process is refused (error path).
import { Page, sleep, INVOKE } from "./cdp.mjs";
const selfPid = +process.argv[4];
const p = await Page.open(async (pg) => (await pg.eval("location.href").catch(() => "")).includes("tauri"));
const fails = [];
const check = (ok, msg) => { console.log(`${ok ? "PASS" : "FAIL"} ${msg}`); if (!ok) fails.push(msg); };
await p.waitFor("document.querySelector('#memcard .pct')");
const snap = await p.eval(`${INVOKE}('get_snapshot')`);
check(snap.stats.total > 4e9, `total RAM ${(snap.stats.total / 2 ** 30).toFixed(1)} GB, used ${snap.stats.usedPct.toFixed(0)}%`);
console.log(`     elevated=${snap.elevated} lists=${snap.stats.lists ? "yes" : "no (expected when not admin)"}`);
const procs = await p.eval(`${INVOKE}('get_processes')`);
check(procs.length > 10, `${procs.length} processes listed, top: ${procs.slice(0, 3).map((x) => x.name).join(", ")}`);
const wsBefore = (await p.eval(`${INVOKE}('get_processes')`)).find((x) => x.pid === selfPid)?.workingSet;
await p.eval(`${INVOKE}('trim_process', { pid: ${selfPid} })`);
await sleep(300);
const all = await p.eval(`${INVOKE}('get_processes')`);
const wsAfter = all.find((x) => x.pid === selfPid)?.workingSet;
check(wsBefore && (wsAfter === undefined || wsAfter < wsBefore), `self trim: ${(wsBefore / 2 ** 20).toFixed(0)} MB -> ${wsAfter === undefined ? "below top 60" : (wsAfter / 2 ** 20).toFixed(0) + " MB"}`);
const rep = await p.eval(`${INVOKE}('run_optimize', { areas: ['standbyLow', 'registryCache'] })`);
console.log("     optimize (non-admin):", rep.results.map((r) => `${r.area}=${r.ok ? "ok" : r.detail}`).join(", "));
check(rep.results.length === 2, "optimize returns a report per step");
const vols = await p.eval(`${INVOKE}('get_volumes')`);
check(vols.length >= 1, `volumes: ${vols.map((v) => `${v.root} ${v.fs} ${(v.free / 2 ** 30).toFixed(0)} GB free`).join("; ")}`);
const pd = await p.eval(`${INVOKE}('get_physical_disks')`).catch((e) => String(e));
check(Array.isArray(pd), `physical disks: ${JSON.stringify(pd).slice(0, 300)}`);
const s = snap.settings; s.excluded = [...s.excluded, "test.exe"];
await p.eval(`${INVOKE}('save_settings', { settings: ${JSON.stringify(s)} })`);
const s2 = (await p.eval(`${INVOKE}('get_snapshot')`)).settings;
check(s2.excluded.includes("test.exe"), "settings round-trip");
await p.shot("win-overview.png");
// --- junk cleaner: scan only (never cleans real data here)
const cats = await p.eval(`${INVOKE}('cleaner_scan')`);
check(cats.length > 10, `cleaner scan: ${cats.filter((c) => c.bytes).map((c) => `${c.id}=${(c.bytes / 2 ** 20).toFixed(0)}MB${c.blocked ? "(blocked)" : ""}`).join(" ")}`);
// --- duplicates on throwaway files in the test folder
const dt = process.cwd() + "\\duptest";
const fs = await import("node:fs");
fs.rmSync(dt, { recursive: true, force: true }); fs.mkdirSync(dt + "\\sub", { recursive: true });
const blob = Buffer.alloc(300 * 1024, 7); const other = Buffer.alloc(300 * 1024, 7); other[200000] = 1;
fs.writeFileSync(dt + "\\a.jpg", blob); fs.writeFileSync(dt + "\\sub\\a copy.jpg", blob); fs.writeFileSync(dt + "\\b.jpg", other);
const groups = await p.eval(`${INVOKE}('dupes_scan', { roots: [${JSON.stringify(dt)}], filter: 'images', minBytes: 1024 })`);
check(groups.length === 1 && groups[0].files.length === 2, `duplicates: ${groups.length} set(s), same-size different file ignored`);
const rm = await p.eval(`${INVOKE}('dupes_remove', { requests: [{ keep: ${JSON.stringify(dt + "\\a.jpg")}, remove: [${JSON.stringify(dt + "\\sub\\a copy.jpg")}, ${JSON.stringify(dt + "\\b.jpg")}] }] })`);
check(rm.recycled === 1 && rm.refused.length === 1 && fs.existsSync(dt + "\\b.jpg") && fs.existsSync(dt + "\\a.jpg"), `recycle: ${rm.recycled} recycled, non-identical file refused`);
// --- health
const h = await p.eval(`${INVOKE}('health_report')`);
const b = h.report?.batteries?.[0];
check(!!b, `battery: ${b ? `${(b.full / b.design * 100).toFixed(1)}% health, ${b.cycles} cycles` : "none"}; ac=${h.power.onAc} ${h.power.percent}%`);
console.log(`     nvme: ${JSON.stringify(h.nvme).slice(0, 200)} | trim=${h.report?.trim} whea=${h.report?.whea30d} plan=${h.report?.plan}`);
const g = await p.eval(`${INVOKE}('gpu_status')`);
check(g.length >= 1, `gpus: ${g.map((x) => `${x.name} util=${x.util.toFixed(0)}% apps=${x.apps.map((a) => a.name).join("/")}`).join(" | ")}`);
const smi = await p.eval(`${INVOKE}('nvidia_sensors')`).catch((e) => String(e));
console.log("     nvidia-smi:", JSON.stringify(smi).slice(0, 300));
for (const t of ["standby", "processes", "cleaner", "dupes", "health", "settings"]) {
  await p.eval(`document.querySelector('nav button[data-tab=${t}]').click()`);
  await sleep(t === "health" ? 9000 : t === "cleaner" ? 5000 : 1200);
  await p.shot(`win-${t}.png`);
}
const errs = await p.eval("document.body.innerText.includes('Error')");
check(!errs, "no error text on pages");
console.log(fails.length ? `${fails.length} FAILED` : "ALL PASSED");
process.exit(fails.length ? 1 : 0);
