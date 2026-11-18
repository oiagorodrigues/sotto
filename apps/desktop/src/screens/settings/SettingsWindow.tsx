import { useState } from "react";
import { Icon, type IconName } from "../../components/Icon";
import { useSnapshot } from "../../lib/snapshot";
import { AccountPage } from "./AccountPage";
import { CalendarPage } from "./CalendarPage";
import { GeneralPage } from "./GeneralPage";
import { ModelsPage } from "./ModelsPage";
import { ModesPage } from "./ModesPage";
import { ShortcutsPage } from "./ShortcutsPage";
import { TranscriptionPage } from "./TranscriptionPage";
import "./settings.css";

const SECTIONS: { id: string; label: string; icon: IconName }[] = [
  { id: "general", label: "General", icon: "gear" },
  { id: "models", label: "Models", icon: "cpu" },
  { id: "transcription", label: "Transcription", icon: "mic" },
  { id: "calendar", label: "Calendar", icon: "calendar" },
  { id: "modes", label: "Modes", icon: "layers" },
  { id: "shortcuts", label: "Shortcuts", icon: "command" },
  { id: "privacy", label: "Privacy", icon: "shield" },
  { id: "account", label: "Account", icon: "user" },
];

/** `#/settings?section=modes` opens a page directly, for screenshots and links. */
function initialSection(): string {
  const wanted = new URLSearchParams(window.location.hash.split("?")[1] ?? "").get("section");
  return SECTIONS.some((s) => s.id === wanted) && wanted ? wanted : "models";
}

export function SettingsWindow() {
  const [section, setSection] = useState(initialSection);
  const snapshot = useSnapshot();
  const current = SECTIONS.find((s) => s.id === section);

  return (
    <div className="win">
      <div className="win-body">
        <nav className="nav" aria-label="Settings sections">
          <ul>
            {SECTIONS.map((s) => (
              <li key={s.id}>
                <button
                  type="button"
                  aria-current={s.id === section ? "page" : undefined}
                  onClick={() => setSection(s.id)}
                >
                  <Icon name={s.icon} />
                  {s.label}
                </button>
              </li>
            ))}
          </ul>
        </nav>
        <div className="pane">
          <main className="content">
            {section === "models" ? (
              snapshot ? (
                <ModelsPage settings={snapshot.settings} notice={snapshot.notice} />
              ) : null
            ) : section === "transcription" ? (
              snapshot ? (
                <TranscriptionPage
                  settings={snapshot.settings}
                  onOpenModels={() => setSection("models")}
                />
              ) : null
            ) : section === "calendar" ? (
              snapshot ? (
                <CalendarPage calendar={snapshot.calendar} />
              ) : null
            ) : section === "general" ? (
              <GeneralPage />
            ) : section === "modes" ? (
              snapshot ? (
                <ModesPage snapshot={snapshot} />
              ) : null
            ) : section === "account" ? (
              snapshot ? (
                <AccountPage account={snapshot.account} />
              ) : null
            ) : section === "shortcuts" ? (
              <ShortcutsPage />
            ) : (
              <>
                <h1>{current?.label}</h1>
                <p className="empty">Nothing to set here yet.</p>
              </>
            )}
          </main>
        </div>
      </div>
    </div>
  );
}
