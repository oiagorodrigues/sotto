import { IconButton } from "../../components/IconButton";
import { SHORTCUTS } from "../../lib/shortcuts";

export function ShortcutsSheet({ onClose }: { onClose: () => void }) {
  return (
    <section className="sheet" aria-label="Keyboard shortcuts">
      <header className="sheet-head">
        <h2>Keyboard shortcuts</h2>
        <IconButton icon="close" label="Close shortcuts" shortcut="Esc" onClick={onClose} />
      </header>
      <ul>
        {SHORTCUTS.map((s) => (
          <li key={s.id}>
            <span>{s.label}</span>
            <span className="keys-group">
              {s.keys.map((k) => (
                <kbd key={k}>{k}</kbd>
              ))}
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}
