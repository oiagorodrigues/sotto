import { Select } from "../../components/Select";
import type { AppSnapshot } from "../../generated/AppSnapshot";
import { dispatch } from "../../lib/snapshot";

const NONE = "";

/** Chooses the mode the next session starts with. A running session keeps its own mode. */
export function ModePicker({ snapshot, listening }: { snapshot: AppSnapshot; listening: boolean }) {
  const { modes, selected_mode_id, session } = snapshot;
  if (listening) {
    return session?.mode_name ? <span className="chip mode">{session.mode_name}</span> : null;
  }
  if (modes.length === 0) return null;
  return (
    <Select
      chip
      label="Mode for the next session"
      value={selected_mode_id ?? NONE}
      options={[
        { value: NONE, label: "No mode" },
        ...modes.map((m) => ({ value: m.id, label: m.name })),
      ]}
      onChange={(id) => void dispatch({ type: "select_mode", mode_id: id === NONE ? null : id })}
    />
  );
}
