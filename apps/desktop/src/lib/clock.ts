let frozenAt: number | null = null;

export function freezeClock(unixMs: number): void {
  frozenAt = unixMs;
}

export function nowMs(): number {
  return frozenAt ?? Date.now();
}
