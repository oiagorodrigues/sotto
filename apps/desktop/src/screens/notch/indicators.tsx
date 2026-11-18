import type { CSSProperties } from "react";

/** Level bars driven by the quantized input level (0 is silence). Styles are in `signals.css`. */
export function Bars({ level, steps }: { level: number; steps: number }) {
  const style = { "--l": Math.min(1, level / steps) } as CSSProperties;
  return (
    <span className="level" data-on={level > 0 || undefined} style={style} aria-hidden="true">
      <i />
      <i />
      <i />
      <i />
      <i />
    </span>
  );
}

export function Dots() {
  return (
    <span className="dots" aria-hidden="true">
      <i />
      <i />
      <i />
    </span>
  );
}
