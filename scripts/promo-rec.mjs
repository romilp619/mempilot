// Records the promo: drives a demo instance over CDP with a visible fake cursor and saves
// screencast frames + scene markers. Usage: node promo-rec.mjs <port> <outdir>
import { Page, sleep } from "./cdp.mjs";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const port = process.argv[2];
const out = process.argv[3];
mkdirSync(join(out, "frames"), { recursive: true });
mkdirSync(join(out, "shots"), { recursive: true });
const p = await Page.open(async (pg) => (await pg.eval("location.href").catch(() => "")).includes("tauri"));
await p.waitFor("document.querySelector('#memcard .pct')");
await p.eval(`document.querySelector('nav button[data-tab=overview]').click()`);

// ---- fake cursor + click ripple, drawn inside the page so it is part of the capture
await p.eval(`(() => {
  const s = document.createElement('style');
  s.textContent = '#dc{position:fixed;left:900px;top:520px;width:26px;height:26px;z-index:99999;pointer-events:none;transition:left .7s cubic-bezier(.4,0,.2,1),top .7s cubic-bezier(.4,0,.2,1);filter:drop-shadow(0 2px 3px rgba(0,0,0,.6))}'
    + '.dr{position:fixed;width:16px;height:16px;margin:-8px 0 0 -8px;border-radius:50%;border:2px solid #2ecca0;z-index:99998;pointer-events:none;animation:dr .5s ease-out forwards}'
    + '@keyframes dr{from{transform:scale(.4);opacity:1}to{transform:scale(2.6);opacity:0}}';
  document.head.appendChild(s);
  const c = document.createElement('div'); c.id = 'dc';
  c.innerHTML = '<svg viewBox="0 0 24 24" width="26" height="26"><path d="M4 2l15 11.5-6.6 1 3.9 7.3-2.9 1.5-3.9-7.4L4 20z" fill="#fff" stroke="#111" stroke-width="1.3" stroke-linejoin="round"/></svg>';
  document.body.appendChild(c);
})()`);

async function moveTo(sel) {
  const ok = await p.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) return false;
    e.scrollIntoView({ block: 'nearest', behavior: 'smooth' }); return true; })()`);
  if (!ok) throw new Error("missing " + sel);
  await sleep(450);
  await p.eval(`(() => { const r = document.querySelector(${JSON.stringify(sel)}).getBoundingClientRect();
    const c = document.getElementById('dc'); c.style.left = (r.left + Math.min(r.width / 2, 60)) + 'px'; c.style.top = (r.top + r.height / 2) + 'px'; })()`);
  await sleep(800);
}
async function click(sel) {
  await moveTo(sel);
  await p.eval(`(() => { const c = document.getElementById('dc'); const d = document.createElement('div'); d.className = 'dr';
    d.style.left = c.style.left; d.style.top = c.style.top; document.body.appendChild(d); setTimeout(() => d.remove(), 600);
    document.querySelector(${JSON.stringify(sel)}).click(); })()`);
  await sleep(350);
}
const scrollMain = (y) => p.eval(`document.querySelector('main').scrollTo({ top: ${y}, behavior: 'smooth' })`);
const scrollEl = (sel, off = 0) => p.eval(`(() => { const m = document.querySelector('main'); const e = document.querySelector(${JSON.stringify(sel)});
  m.scrollTo({ top: m.scrollTop + e.getBoundingClientRect().top - 20 + ${off}, behavior: 'smooth' }); })()`);

// ---- screencast
const frames = [];
p.ws.addEventListener("message", (m) => {
  const msg = JSON.parse(m.data);
  if (msg.method !== "Page.screencastFrame") return;
  const { data, metadata, sessionId } = msg.params;
  const name = `f${String(frames.length).padStart(5, "0")}.jpg`;
  writeFileSync(join(out, "frames", name), Buffer.from(data, "base64"));
  frames.push({ name, t: metadata.timestamp });
  p.send("Page.screencastFrameAck", { sessionId }).catch(() => {});
});
const scenes = [];
let cur = null;
const begin = (id, caption) => { cur = { id, caption, start: Date.now() / 1000 }; };
const end = () => { cur.end = Date.now() / 1000; scenes.push(cur); };
async function shot(name) {
  await p.eval("document.getElementById('dc').style.opacity = 0");
  await sleep(120);
  await p.shot("shots/" + name);
  await p.eval("document.getElementById('dc').style.opacity = 1");
}

await sleep(9000); // let the 2-minute graph collect some history
await p.send("Page.startScreencast", { format: "jpeg", quality: 92, everyNthFrame: 1 });
await sleep(600);

// 1. overview
begin("overview", "See exactly what is in your RAM");
await sleep(3800);
end();
await shot("01-memory.png");

// 2. optimize (Deep)
begin("optimize", "One click frees memory, with honest before and after numbers");
await scrollEl(".presets");
await sleep(700);
await click('[data-preset="deep"]');
await click("#go");
await p.waitFor("!document.querySelector('#go').disabled && document.querySelector('#result .result')", 60000);
await sleep(300);
await scrollEl("#result", -120);
await sleep(3200);
end();
await shot("02-optimize-result.png");

// 3. standby
begin("standby", "Standby cache by priority, plus an automatic standby cleaner");
await click('nav button[data-tab="standby"]');
await sleep(3000);
end();
await shot("03-standby.png");

// 4. junk cleaner (scan only)
await p.eval(`document.querySelector('nav button[data-tab="processes"]').click()`);
await sleep(200);
begin("junk", "Junk cleaner: temp files, browser caches and package caches");
await click('nav button[data-tab="cleaner"]');
await p.waitFor("document.querySelector('#clean-go')", 60000);
await sleep(1600);
await scrollMain(420);
await sleep(2400);
end();
await scrollMain(0);
await sleep(700);
await shot("04-junk-cleaner.png");

// 5. duplicates (demo photos only)
begin("dupes", "Finds duplicate photos. Extra copies go to the Recycle Bin");
await click('nav button[data-tab="dupes"]');
await click('[data-filter="images"]');
await p.eval(`(() => { const s = document.querySelector('#minsize'); s.value = '102400'; s.dispatchEvent(new Event('change')); })()`);
await click("#dupscan");
await p.waitFor("document.querySelector('.dupgroup') || document.body.innerText.includes('No duplicates')", 60000);
await sleep(800);
await scrollEl(".sticky-go", -10);
await sleep(3200);
end();
await shot("05-duplicates.png");

// 6. laptop health
await click('nav button[data-tab="health"]');
await p.waitFor("document.querySelector('#hrefresh')", 90000);
await sleep(600);
begin("health", "Laptop health: battery wear, SSD life, apps keeping the GPU awake");
await sleep(1800);
await shot("06-health-battery.png");
await scrollEl("#tab-health .card:nth-of-type(3)", -10);
await sleep(2400);
end();
await shot("07-health-ssd.png");
await scrollEl("#tab-health .card:nth-of-type(4)", -10);
await sleep(1500);
await shot("08-health-gpu.png");

await p.send("Page.stopScreencast");
await sleep(500);
writeFileSync(join(out, "timeline.json"), JSON.stringify({ frames, scenes }, null, 1));
console.log(`frames=${frames.length}`, scenes.map((s) => `${s.id}:${(s.end - s.start).toFixed(1)}s`).join(" "));
process.exit(0);
