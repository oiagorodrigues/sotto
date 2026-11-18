import { useState } from "react";
import { Dialog } from "../../components/Dialog";
import { Icon } from "../../components/Icon";
import type { SettingsView } from "../../generated/SettingsView";
import { dispatch } from "../../lib/snapshot";
import "./engine-picker.css";

interface Props {
  settings: SettingsView;
  onOpenModels: () => void;
}

export function EnginePicker({ settings, onOpenModels }: Props) {
  const [dialog, setDialog] = useState<"confirm" | "needs-key" | null>(null);
  const cloud = settings.engine_preference === "cloud";
  const ownKey = settings.providers.some((p) => p.provider === "openai" && p.configured);
  const choose = (engine: "local" | "cloud") => {
    if (engine === "local") {
      void dispatch({ type: "set_engine_preference", engine });
    } else if (!cloud) {
      setDialog(settings.cloud_transcription_available ? "confirm" : "needs-key");
    }
  };
  return (
    <section className="mod" aria-labelledby="engine-name">
      <div className="mod-h">
        <h3 id="engine-name">Engine</h3>
        <span className="status">
          <span className={cloud ? "led on" : "led"} aria-hidden="true" />
          {cloud ? "Cloud" : "On this Mac"}
        </span>
      </div>
      <div className="mod-b">
        <div role="radiogroup" aria-labelledby="engine-name" className="engine-choice">
          <Choice
            icon="cpu"
            label="On this Mac"
            checked={!cloud}
            onSelect={() => choose("local")}
          />
          <Choice icon="cloud" label="Cloud" checked={cloud} onSelect={() => choose("cloud")} />
        </div>
        {cloud && !settings.cloud_transcription_available ? (
          <p className="engine-warn" role="alert">
            <Icon name="alert" size={12} />
            Sessions will not start until you add an OpenAI key or sign in.
          </p>
        ) : null}
      </div>
      {dialog === "confirm" && (
        <Dialog
          title="Send call audio to OpenAI?"
          onClose={() => setDialog(null)}
          footer={
            <>
              <button type="button" className="btn" data-autofocus onClick={() => setDialog(null)}>
                Cancel
              </button>
              <button
                type="button"
                className="btn primary"
                onClick={() => {
                  void dispatch({ type: "set_engine_preference", engine: "cloud" });
                  setDialog(null);
                }}
              >
                Use cloud transcription
              </button>
            </>
          }
        >
          <p>
            While you listen, the speech of everyone on the call, including the other participants,
            is sent to OpenAI to be transcribed. Sotto never switches to this on its own.
          </p>
          <p>
            {ownKey
              ? "Your OpenAI key is billed for the minutes of speech sent."
              : "Sotto's hosted transcription is used because no OpenAI key is saved."}
          </p>
        </Dialog>
      )}
      {dialog === "needs-key" && (
        <Dialog
          title="Add an OpenAI key or sign in first"
          onClose={() => setDialog(null)}
          footer={
            <>
              <button type="button" className="btn" onClick={() => setDialog(null)}>
                Cancel
              </button>
              <button
                type="button"
                className="btn primary"
                data-autofocus
                onClick={() => {
                  setDialog(null);
                  onOpenModels();
                }}
              >
                Open Models
              </button>
            </>
          }
        >
          <p>
            Cloud transcription needs an OpenAI key saved in Models, or a Sotto account signed in
            under Account.
          </p>
        </Dialog>
      )}
    </section>
  );
}

function Choice({
  icon,
  label,
  checked,
  onSelect,
}: {
  icon: "cpu" | "cloud";
  label: string;
  checked: boolean;
  onSelect: () => void;
}) {
  return (
    // biome-ignore lint/a11y/useSemanticElements: a two-option radio group styled as buttons
    <button
      type="button"
      role="radio"
      aria-checked={checked}
      className="engine-opt"
      onClick={onSelect}
    >
      <Icon name={icon} size={16} />
      {label}
    </button>
  );
}
