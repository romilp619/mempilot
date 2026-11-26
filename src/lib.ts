import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import { mock } from "./mock";

const isTauri = "__TAURI_INTERNALS__" in window;
export const invoke = <T>(cmd: string, args?: Record<string, unknown>): Promise<T> =>
  isTauri ? tauriInvoke<T>(cmd, args) : mock.invoke<T>(cmd, args);
export const listen = <T>(ev: string, cb: (p: T) => void) =>
  isTauri ? tauriListen<T>(ev, (e) => cb(e.payload)) : mock.listen<T>(ev, cb);

export const $ = <T extends HTMLElement = HTMLElement>(sel: string, root: ParentNode = document) => root.querySelector(sel) as T;
export const esc = (s: string) =>
  s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
export function fmt(b: number, d = 1) {
  const a = Math.abs(b);
  if (a >= 1000 * 1024 ** 3) return (b / 1024 ** 4).toFixed(2) + " TB";
  if (a >= 1024 ** 3) return (b / 1024 ** 3).toFixed(d) + " GB";
  if (a >= 1024 ** 2) return (b / 1024 ** 2).toFixed(0) + " MB";
  return (b / 1024).toFixed(0) + " KB";
}
export const signed = (b: number) => (b >= 0 ? "+" : "−") + fmt(Math.abs(b));
export const when = (ms: number) => new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
export function toast(msg: string) {
  const t = $("#toast");
  t.textContent = msg;
  t.classList.add("show");
  clearTimeout((t as any)._h);
  (t as any)._h = setTimeout(() => t.classList.remove("show"), 3600);
}
