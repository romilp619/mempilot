// Screenshots every tab in headless Chromium against the dev server + mock backend.
import { chromium } from "playwright-core";
import { spawn } from "node:child_process";
import { existsSync, readdirSync, mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
const out = process.argv[2] ?? "shots";
mkdirSync(out, { recursive: true });
const cache = join(homedir(), ".cache/ms-playwright");
const exe = readdirSync(cache).filter((d) => /^chromium-\d+$/.test(d)).sort().reverse()
  .map((d) => join(cache, d, "chrome-linux64/chrome")).find(existsSync);
const server = spawn(process.execPath, ["node_modules/vite/bin/vite.js"], { stdio: "pipe" });
await new Promise((res, rej) => { server.stdout.on("data", (d) => String(d).includes("Local") && res()); setTimeout(() => rej(new Error("vite")), 20000); });
const browser = await chromium.launch({ executablePath: exe });
const page = await browser.newPage({ viewport: { width: 1080, height: 760 } });
const problems = [];
page.on("console", (m) => m.type() === "error" && problems.push(m.text()));
page.on("pageerror", (e) => problems.push(e.message));
try {
  await page.goto("http://localhost:1430/");
  await page.waitForTimeout(4000);
  await page.screenshot({ path: join(out, "overview.png"), fullPage: true });
  await page.click("#go"); await page.waitForTimeout(1600);
  await page.evaluate(() => document.querySelector("main").scrollTo(0, 9999));
  await page.screenshot({ path: join(out, "overview-result.png") });
  for (const t of ["standby", "processes", "disks", "settings"]) {
    await page.click(`nav button[data-tab=${t}]`); await page.waitForTimeout(700);
    await page.screenshot({ path: join(out, `${t}.png`) });
  }
} finally { await browser.close(); server.kill(); }
console.log(problems.length ? problems.join("\n") : "no console errors");
