import { Icon } from "../../components/Icon";
import "./engine-mark.css";

/** Shown only while transcription runs in the cloud: that audio leaves the Mac, so it is never hidden. */
export function EngineMark({ expanded }: { expanded: boolean }) {
  const label = "Transcribing in the cloud";
  return (
    <span className="eng" role="img" aria-label={label} title={expanded ? undefined : label}>
      <Icon name="cloud" size={12} />
      {expanded ? "Cloud" : null}
    </span>
  );
}
