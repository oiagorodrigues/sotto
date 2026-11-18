import type { ReactNode } from "react";

interface Props {
  onBack?: () => void;
  /** Buttons on the right, secondary first. The primary action goes last. */
  children: ReactNode;
}

export function StepFooter({ onBack, children }: Props) {
  return (
    <div className="onb-foot">
      {onBack ? (
        <button type="button" className="btn ghost" onClick={onBack}>
          Back
        </button>
      ) : (
        <span />
      )}
      <div className="r">{children}</div>
    </div>
  );
}
