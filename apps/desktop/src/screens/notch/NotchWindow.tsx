import { useCallback, useEffect, useRef, useState } from "react";
import { applyMaxHeight, useReportSize } from "../../lib/geometry";
import { useSnapshot } from "../../lib/snapshot";
import { isTauri, subscribeShortcut } from "../../lib/tauri";
import { CompactBar } from "./CompactBar";
import { Panel } from "./Panel";
import { ResizeGrip } from "./ResizeGrip";
import { MeetingStrip } from "./UpcomingMeeting";
import "./notch.css";

const WIDTH_KEY = "sotto.notch.width";
export const MIN_WIDTH = 440;
export const MAX_WIDTH = 760;
const DEFAULT_WIDTH = 600;

const clampWidth = (w: number): number => Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, Math.round(w)));

function loadWidth(): number {
  try {
    const stored = Number(window.localStorage.getItem(WIDTH_KEY));
    return Number.isFinite(stored) && stored > 0 ? clampWidth(stored) : DEFAULT_WIDTH;
  } catch {
    return DEFAULT_WIDTH;
  }
}

function saveWidth(width: number): void {
  try {
    window.localStorage.setItem(WIDTH_KEY, String(width));
  } catch {
    // Storage can be blocked; the width then resets on the next launch.
  }
}

/** Browser fixtures open straight into a state, e.g. `#/notch?expanded=1&sheet=1`. */
function previewFlags(): { expanded: boolean; sheet: boolean } {
  if (isTauri()) return { expanded: false, sheet: false };
  const params = new URLSearchParams(window.location.hash.split("?")[1] ?? "");
  return { expanded: params.get("expanded") === "1", sheet: params.get("sheet") === "1" };
}

export function NotchWindow() {
  const snapshot = useSnapshot();
  const surface = useRef<HTMLDivElement | null>(null);
  const preview = previewFlags();
  const [expanded, setExpanded] = useState(preview.expanded);
  const [sheetOpen, setSheetOpen] = useState(preview.sheet);
  const [width, setWidth] = useState(loadWidth);
  const [askTick, setAskTick] = useState(0);
  const [askRequest, setAskRequest] = useState(0);
  const askHandled = useRef(0);
  const autoExpanded = useRef<string | null>(null);

  useReportSize(surface);
  useEffect(() => {
    document.documentElement.classList.add("floating");
    applyMaxHeight();
    return () => document.documentElement.classList.remove("floating");
  }, []);

  const collapse = useCallback(() => {
    setExpanded(false);
    setSheetOpen(false);
  }, []);

  const answerId = snapshot?.answer?.id ?? null;
  const answerState = snapshot?.answer?.state ?? null;
  const answerOrigin = snapshot?.answer?.origin ?? null;
  useEffect(() => {
    if (answerId && answerState === "streaming" && answerOrigin === "auto") {
      if (autoExpanded.current !== answerId) {
        autoExpanded.current = answerId;
        setExpanded(true);
      }
    }
  }, [answerId, answerState, answerOrigin]);

  useEffect(
    () =>
      subscribeShortcut((name) => {
        if (name === "ask") {
          setExpanded(true);
          setAskRequest((n) => n + 1);
        } else if (name === "toggle") {
          setExpanded((open) => {
            if (open) setSheetOpen(false);
            return !open;
          });
        }
      }),
    [],
  );

  const isOpen = expanded && snapshot !== null;
  useEffect(() => {
    if (isOpen && askRequest !== askHandled.current) {
      askHandled.current = askRequest;
      setAskTick((t) => t + 1);
    }
  }, [isOpen, askRequest]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      if (sheetOpen) setSheetOpen(false);
      else collapse();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [sheetOpen, collapse]);

  return (
    <div className="notch-root">
      <div
        ref={surface}
        className="surface"
        data-expanded={isOpen || undefined}
        style={{ "--panel-w": `${width}px` } as React.CSSProperties}
      >
        <Ear side="l" />
        <Ear side="r" />
        <div className="body">
          {snapshot ? (
            <CompactBar
              snapshot={snapshot}
              expanded={isOpen}
              onExpand={() => setExpanded(true)}
              onCollapse={collapse}
            />
          ) : (
            <div className="bar" />
          )}
          {!isOpen && snapshot ? <MeetingStrip snapshot={snapshot} /> : null}
          {isOpen && snapshot ? (
            <>
              <Panel
                snapshot={snapshot}
                askTick={askTick}
                sheetOpen={sheetOpen}
                onSheet={setSheetOpen}
              />
              <ResizeGrip
                width={width}
                min={MIN_WIDTH}
                max={MAX_WIDTH}
                onChange={(w) => setWidth(clampWidth(w))}
                onCommit={saveWidth}
              />
            </>
          ) : null}
        </div>
      </div>
    </div>
  );
}

/** Concave corner where the black meets the top edge. Half a pixel overlaps the body so no seam shows. */
function Ear({ side }: { side: "l" | "r" }) {
  return (
    <svg
      className={`ear ${side}`}
      viewBox="0 0 12.5 12"
      width="12.5"
      height="12"
      aria-hidden="true"
      focusable="false"
    >
      <path
        d={side === "l" ? "M0 0H12.5V12H12A12 12 0 0 0 0 0Z" : "M0 0H12.5A12 12 0 0 0 .5 12H0Z"}
      />
    </svg>
  );
}
