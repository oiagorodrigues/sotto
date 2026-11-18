import type { CalendarEventView } from "../generated/CalendarEventView";
import type { CalendarView } from "../generated/CalendarView";
import { formatClock } from "./format";

const MINUTE = 60_000;

/** "in 2 min", "now", "started 4 min ago". */
export function relativeStart(startMs: number, nowMs: number): string {
  const diff = startMs - nowMs;
  if (Math.abs(diff) < MINUTE) return "now";
  const minutes = Math.round(Math.abs(diff) / MINUTE);
  if (diff > 0) {
    if (minutes < 60) return `in ${minutes} min`;
    return `at ${formatClock(startMs)}`;
  }
  return `started ${minutes} min ago`;
}

export function promptEvent(calendar: CalendarView): CalendarEventView | null {
  if (calendar.prompt_event_id === null) return null;
  return calendar.events.find((e) => e.id === calendar.prompt_event_id) ?? null;
}

/** The meeting to show in the expanded idle panel: the prompt, else the first one not over. */
export function nextEvent(calendar: CalendarView, nowMs: number): CalendarEventView | null {
  return promptEvent(calendar) ?? calendar.events.find((e) => e.end_ms > nowMs) ?? null;
}

export function looksLikeClientId(value: string): boolean {
  return value.length >= 10 && value.endsWith(".apps.googleusercontent.com") && !/\s/.test(value);
}

export function looksLikeClientSecret(value: string): boolean {
  return value.length >= 10 && !/\s/.test(value);
}
