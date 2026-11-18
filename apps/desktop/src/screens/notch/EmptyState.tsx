import { Icon } from "../../components/Icon";
import type { AppSnapshot } from "../../generated/AppSnapshot";
import { toggleListening } from "./CompactBar";

export function EmptyState({ snapshot }: { snapshot: AppSnapshot }) {
  return (
    <section className="empty-state" aria-label="Not listening">
      <span className="empty-mic" aria-hidden="true">
        <Icon name="mic" size={20} />
      </span>
      <p className="empty-title">Start listening to get answers</p>
      <p className="empty-how">
        Press the mic button or <kbd>⌘</kbd>
        <kbd>⇧</kbd>
        <kbd>L</kbd>
      </p>
      <button
        type="button"
        className="btn primary sm"
        disabled={snapshot.phase === "onboarding"}
        onClick={() => void toggleListening(snapshot)}
      >
        <Icon name="mic" size={14} />
        Start listening
      </button>
    </section>
  );
}
