import { Select } from "../../components/Select";
import type { SettingsView } from "../../generated/SettingsView";
import { dispatch } from "../../lib/snapshot";
import { EnginePicker } from "./EnginePicker";
import { TranscriptionCheckModule } from "./TranscriptionCheckModule";

interface Props {
  settings: SettingsView;
  onOpenModels: () => void;
}

function localeLabel(tag: string): string {
  try {
    const name = new Intl.DisplayNames(["en"], { type: "language" }).of(tag);
    return name ? `${name} (${tag})` : tag;
  } catch {
    return tag;
  }
}

export function TranscriptionPage({ settings, onOpenModels }: Props) {
  const tags = settings.available_locales.includes(settings.locale)
    ? settings.available_locales
    : [settings.locale, ...settings.available_locales];
  return (
    <>
      <h1>Transcription</h1>
      <p className="sub">The language of the meeting audio.</p>
      <div className="mods">
        <section className="mod" aria-labelledby="language-name">
          <div className="mod-h">
            <h3 id="language-name">Language</h3>
          </div>
          <div className="mod-b">
            <Select
              label="Spoken language"
              options={tags.map((tag) => ({ value: tag, label: localeLabel(tag) }))}
              value={settings.locale}
              onChange={(locale) => void dispatch({ type: "set_locale", locale })}
            />
          </div>
        </section>
        <EnginePicker settings={settings} onOpenModels={onOpenModels} />
      </div>
      <TranscriptionCheckModule />
    </>
  );
}
