import { useState } from "react";
import { Icon } from "./Icon";
import { IconButton } from "./IconButton";
import "./notice.css";

/** Latest app error from the core. Dismissing hides that text until a different one arrives. */
export function NoticeBar({ notice }: { notice: string | null }) {
  const [dismissed, setDismissed] = useState<string | null>(null);
  if (notice === null || notice === dismissed) return null;
  return (
    <div className="notice" role="alert">
      <Icon name="alert" size={14} />
      <p>{notice}</p>
      <IconButton icon="close" label="Dismiss" onClick={() => setDismissed(notice)} />
    </div>
  );
}
