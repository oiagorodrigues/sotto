import { useEffect, useState } from "react";
import type { AppSnapshot } from "../generated/AppSnapshot";
import type { Intent } from "../generated/Intent";
import { dispatch as send, subscribeSnapshot } from "./tauri";

export function useSnapshot(): AppSnapshot | null {
  const [snapshot, setSnapshot] = useState<AppSnapshot | null>(null);
  useEffect(() => subscribeSnapshot(setSnapshot), []);
  return snapshot;
}

export function dispatch(intent: Intent): Promise<void> {
  return send(intent);
}
