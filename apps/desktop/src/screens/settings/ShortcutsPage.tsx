import { SHORTCUTS } from "../../lib/shortcuts";

export function ShortcutsPage() {
  return (
    <>
      <h1>Shortcuts</h1>
      <p className="sub">Rebinding is not available yet.</p>
      <div className="mods">
        <section className="mod" aria-labelledby="shortcuts-name">
          <div className="mod-h">
            <h3 id="shortcuts-name">Overlay</h3>
          </div>
          <ul className="mod-b shortcut-list">
            {SHORTCUTS.map((s) => (
              <li key={s.id}>
                <span>{s.label}</span>
                <span className="shortcut-meta">
                  {s.global ? null : <span className="chip">In the panel</span>}
                  <span className="keys-group">
                    {s.keys.map((k) => (
                      <kbd key={k}>{k}</kbd>
                    ))}
                  </span>
                </span>
              </li>
            ))}
          </ul>
        </section>
      </div>
    </>
  );
}
