import { useEffect, useState } from "react";
import { Icon } from "../../components/Icon";
import { NoticeBar } from "../../components/NoticeBar";
import { Select } from "../../components/Select";
import type { SettingsView } from "../../generated/SettingsView";
import { dispatch } from "../../lib/snapshot";
import { capToText, modelOptions, parseCap, provider } from "./models";
import { ProviderModule } from "./ProviderModule";

interface Props {
  settings: SettingsView;
  notice: string | null;
}

export function ModelsPage({ settings, notice }: Props) {
  const ollama = provider(settings, "ollama");
  const installed = ollama?.models.length ?? 0;
  const setModels = (auto_answer_model: string, manual_model: string) =>
    void dispatch({ type: "set_models", auto_answer_model, manual_model });
  return (
    <>
      <h1>Models</h1>
      <p className="sub">Providers that write your answers.</p>
      <NoticeBar notice={notice} />
      <div className="mods">
        <ProviderModule id="anthropic" name="Anthropic" view={provider(settings, "anthropic")} />
        <ProviderModule id="openai" name="OpenAI" view={provider(settings, "openai")} />
        <section className="mod" aria-labelledby="ollama-name">
          <div className="mod-h">
            <h3 id="ollama-name">Ollama</h3>
            <span className="status">
              <span className={settings.ollama_running ? "led on" : "led"} aria-hidden="true" />
              {settings.ollama_running ? "Running locally" : "Not running"}
            </span>
          </div>
          <div className="mod-b">
            <p className="free">
              <b>Free</b>
              <span>
                {settings.ollama_running
                  ? `${installed} ${installed === 1 ? "model" : "models"} installed`
                  : "Start Ollama to use local models"}
              </span>
            </p>
          </div>
        </section>
        <section className="mod" aria-labelledby="answers-name">
          <div className="mod-h">
            <h3 id="answers-name">Answers</h3>
          </div>
          <div className="mod-b">
            <Select
              label="Auto answers"
              options={modelOptions(settings, settings.auto_answer_model)}
              value={settings.auto_answer_model}
              onChange={(m) => setModels(m, settings.manual_model)}
            />
            <Select
              label="Manual asks"
              options={modelOptions(settings, settings.manual_model)}
              value={settings.manual_model}
              onChange={(m) => setModels(settings.auto_answer_model, m)}
            />
          </div>
        </section>
      </div>
      <SpendCap microUsd={settings.spend_cap_micro_usd} />
    </>
  );
}

function SpendCap({ microUsd }: { microUsd: number }) {
  const [text, setText] = useState(capToText(microUsd));
  useEffect(() => setText(capToText(microUsd)), [microUsd]);
  const parsed = parseCap(text);
  const commit = () => {
    if (parsed !== null && parsed !== microUsd)
      void dispatch({ type: "set_spend_cap", micro_usd: parsed });
  };
  return (
    <section className="mod cap-mod" aria-labelledby="spend-name">
      <div className="mod-h">
        <h3 id="spend-name">Spend</h3>
      </div>
      <div className="mod-b">
        <div className="field cap-field">
          <label htmlFor="cap">Auto-answer spend cap per session</label>
          <div className="money">
            <span className="mono" aria-hidden="true">
              $
            </span>
            <input
              id="cap"
              className="input mono"
              inputMode="decimal"
              value={text}
              aria-invalid={parsed === null ? true : undefined}
              aria-describedby="cap-error"
              onChange={(e) => setText(e.target.value)}
              onBlur={commit}
              onKeyDown={(e) => e.key === "Enter" && commit()}
            />
          </div>
          {parsed === null ? (
            <p className="field-error" id="cap-error" role="alert">
              <Icon name="alert" size={12} />
              Enter an amount from 0 to 50.
            </p>
          ) : null}
        </div>
      </div>
    </section>
  );
}
