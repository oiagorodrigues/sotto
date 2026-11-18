import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef } from "react";
import { isTauri } from "./tauri";

export interface NotchGeometry {
  notch_width: number;
  notch_height: number;
}

export function applyGeometry(g: NotchGeometry): void {
  const root = document.documentElement;
  root.style.setProperty("--notch-w", `${g.notch_width}px`);
  root.style.setProperty("--notch-h", `${g.notch_height}px`);
  root.dataset.notch = String(g.notch_width > 0);
}

/** Share of the screen height the expanded panel may use. */
export const MAX_HEIGHT_SHARE = 0.7;

export function applyMaxHeight(): void {
  document.documentElement.style.setProperty(
    "--max-h",
    `${Math.round(window.screen.height * MAX_HEIGHT_SHARE)}px`,
  );
}

/**
 * Reports the size of `target` to the shell, which sizes the native window to match. The shell
 * answers with the hardware notch geometry, which the layout depends on.
 */
export function useReportSize(target: React.RefObject<HTMLElement | null>): void {
  const last = useRef("");
  useEffect(() => {
    const el = target.current;
    if (!el || !isTauri()) return;
    let frame = 0;
    const report = () => {
      frame = 0;
      const width = Math.ceil(el.offsetWidth);
      const height = Math.ceil(el.offsetHeight);
      const key = `${width}x${height}`;
      if (key === last.current) return;
      last.current = key;
      invoke<NotchGeometry>("report_notch_size", { width, height })
        .then(applyGeometry)
        .catch(() => {
          last.current = "";
        });
    };
    const schedule = () => {
      if (frame === 0) frame = requestAnimationFrame(report);
    };
    const observer = new ResizeObserver(schedule);
    observer.observe(el);
    schedule();
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [target]);
}
