import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AppSnapshot } from "../generated/AppSnapshot";
import type { Intent } from "../generated/Intent";
import * as fixture from "./fixture";

export const SNAPSHOT_EVENT = "sotto://snapshot";
export const SHORTCUT_EVENT = "sotto://shortcut";

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function subscribeSnapshot(onSnapshot: (s: AppSnapshot) => void): () => void {
  if (!isTauri()) return fixture.subscribe(onSnapshot);
  let unlisten: UnlistenFn | undefined;
  let cancelled = false;
  let gotEvent = false;
  listen<AppSnapshot>(SNAPSHOT_EVENT, (e) => {
    gotEvent = true;
    onSnapshot(e.payload);
  }).then(async (fn) => {
    if (cancelled) {
      fn();
      return;
    }
    unlisten = fn;
    const current = await invoke<AppSnapshot | null>("current_snapshot");
    if (current && !gotEvent && !cancelled) onSnapshot(current);
  });
  return () => {
    cancelled = true;
    unlisten?.();
  };
}

export function subscribeShortcut(onShortcut: (name: string) => void): () => void {
  if (!isTauri()) return () => {};
  let unlisten: UnlistenFn | undefined;
  let cancelled = false;
  listen<string>(SHORTCUT_EVENT, (e) => onShortcut(e.payload)).then((fn) => {
    if (cancelled) fn();
    else unlisten = fn;
  });
  return () => {
    cancelled = true;
    unlisten?.();
  };
}

export async function dispatch(intent: Intent): Promise<void> {
  if (!isTauri()) return fixture.dispatch(intent);
  await invoke("dispatch", { intent });
}
