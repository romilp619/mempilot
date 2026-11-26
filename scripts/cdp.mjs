// Minimal Chrome DevTools Protocol client for driving the WebView2 inside Swoop (no dependencies).
import { writeFileSync } from "node:fs";
import { join } from "node:path";

export const port = process.argv[2] ?? "9333";
export const out = process.argv[3] ?? ".";
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const INVOKE = "window.__TAURI_INTERNALS__.invoke";

async function targets() {
  return (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).filter((t) => t.type === "page");
}

export class Page {
  static async open(match, timeout = 15000) {
    const until = Date.now() + timeout;
    for (;;) {
      const list = await targets().catch(() => []);
      for (const t of list) {
        const p = new Page(t);
        await p.ready;
        if (await match(p)) return p;
        p.ws.close();
      }
      if (Date.now() > until) throw new Error("page not found");
      await sleep(300);
    }
  }
  constructor(t) {
    this.id = 0;
    this.pending = new Map();
    this.ws = new WebSocket(t.webSocketDebuggerUrl);
    this.ready = new Promise((res, rej) => {
      this.ws.onopen = res;
      this.ws.onerror = rej;
    });
    this.ws.onmessage = (m) => {
      const msg = JSON.parse(m.data);
      const p = this.pending.get(msg.id);
      if (p) {
        this.pending.delete(msg.id);
        msg.error ? p.rej(new Error(msg.error.message)) : p.res(msg.result);
      }
    };
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((res, rej) => {
      this.pending.set(id, { res, rej });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async eval(expression) {
    const r = await this.send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true, userGesture: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
    return r.result.value;
  }
  async waitFor(expression, timeout = 20000) {
    const until = Date.now() + timeout;
    for (;;) {
      const v = await this.eval(expression).catch(() => null);
      if (v) return v;
      if (Date.now() > until) throw new Error(`timed out waiting for: ${expression}`);
      await sleep(250);
    }
  }
  async shot(name) {
    const r = await this.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, name), Buffer.from(r.data, "base64"));
  }
}


export const byLabel = (label) => async (p) =>
  (await p.eval("window.__TAURI_INTERNALS__?.metadata?.currentWindow?.label").catch(() => null)) === label;
