const pad = (n: number): string => String(n).padStart(2, "0");

export function formatTimer(elapsedMs: number): string {
  const total = Math.max(0, Math.floor(elapsedMs / 1000));
  return `${pad(Math.floor(total / 60))}:${pad(total % 60)}`;
}

export function formatClock(unixMs: number): string {
  const d = new Date(unixMs);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
