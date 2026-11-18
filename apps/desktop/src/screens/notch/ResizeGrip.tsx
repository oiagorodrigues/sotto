import { useRef } from "react";

interface Props {
  width: number;
  min: number;
  max: number;
  onChange: (width: number) => void;
  onCommit: (width: number) => void;
}

const KEY_STEP = 20;

/**
 * Bottom-right corner handle. The window stays centered on the notch, so the width changes by
 * twice the pointer movement to keep the corner under the cursor.
 */
export function ResizeGrip({ width, min, max, onChange, onCommit }: Props) {
  const drag = useRef<{ x: number; width: number } | null>(null);
  const latest = useRef(width);
  latest.current = width;

  const move = (w: number) => {
    const next = Math.min(max, Math.max(min, Math.round(w)));
    latest.current = next;
    onChange(next);
  };

  return (
    <div
      className="grip"
      role="slider"
      tabIndex={0}
      aria-label="Panel width"
      aria-orientation="horizontal"
      aria-valuemin={min}
      aria-valuemax={max}
      aria-valuenow={width}
      onPointerDown={(e) => {
        e.currentTarget.setPointerCapture(e.pointerId);
        drag.current = { x: e.screenX, width };
      }}
      onPointerMove={(e) => {
        if (drag.current) move(drag.current.width + 2 * (e.screenX - drag.current.x));
      }}
      onPointerUp={() => {
        if (drag.current) onCommit(latest.current);
        drag.current = null;
      }}
      onPointerCancel={() => {
        drag.current = null;
      }}
      onKeyDown={(e) => {
        if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
        e.preventDefault();
        move(width + (e.key === "ArrowRight" ? KEY_STEP : -KEY_STEP));
        onCommit(latest.current);
      }}
    >
      <svg
        className="grip-mark"
        viewBox="0 0 12 12"
        width="12"
        height="12"
        aria-hidden="true"
        focusable="false"
      >
        <path d="M11 4 4 11M11 8 8 11" />
      </svg>
    </div>
  );
}
