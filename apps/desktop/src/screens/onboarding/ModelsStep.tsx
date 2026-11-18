import { Icon } from "../../components/Icon";
import { Select } from "../../components/Select";
import type { AppSnapshot } from "../../generated/AppSnapshot";
import { dispatch } from "../../lib/snapshot";
import { modelLabel, modelOptions, provider } from "../settings/models";
import { ProviderModule } from "../settings/ProviderModule";
import { StepFooter } from "./StepFooter";

export function ModelsStep({
  snapshot,
  onBack,
  onContinue,
}: {
  snapshot: AppSnapshot;
  onBack: () => void;
  onContinue: () => void;
}) {
  const { settings, onboarding } = snapshot;
  const test = onboarding.model_test;
  const ollama = provider(settings, "ollama");
  const installed = ollama?.models.length ?? 0;
  const anyReady = settings.providers.some((p) => p.configured);
  const setModels = (auto_answer_model: string, manual_model: string) =>
    void dispatch({ type: "set_models", auto_answer_model, manual_model });
  return (
    <>
      <div className="mods">
        <ProviderModule id="anthropic" name="Anthropic" view={provider(settings, "anthropic")} />
        <ProviderModule id="openai" name="OpenAI" view={provider(settings, "openai")} />
        <section className="mod" aria-labelledby="onb-ollama">
          <div className="mod-h">
            <h3 id="onb-ollama">Ollama</h3>
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
        <section className="mod" aria-labelledby="onb-answers">
          <div className="mod-h">
            <h3 id="onb-answers">Answers</h3>
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
        <section className="mod" aria-labelledby="onb-test">
          <div className="mod-h">
            <h3 id="onb-test">Test</h3>
            <span className="status" role="status">
              {test.status === "passed" ? (
                <>
                  <span className="led on" aria-hidden="true" />
                  <span className="mono">{test.latency_ms} ms</span>
                </>
              ) : null}
              {test.status === "running" ? "Waiting for the model" : null}
            </span>
          </div>
          <div className="mod-b">
            <p className="free">
              <b>{modelLabel(settings.manual_model)}</b>
              <span>Sends one short message. It may cost a fraction of a cent.</span>
            </p>
            {test.status === "failed" ? (
              <p className="field-error" role="alert">
                <Icon name="alert" size={12} />
                {test.error}
              </p>
            ) : null}
            <div className="row">
              <button
                type="button"
                className="btn sm"
                disabled={test.status === "running" || !anyReady}
                onClick={() => void dispatch({ type: "test_model" })}
              >
                Test model
              </button>
            </div>
          </div>
        </section>
      </div>
      <StepFooter onBack={onBack}>
        <button type="button" className="btn primary" onClick={onContinue}>
          {anyReady ? "Continue" : "Skip for now"}
        </button>
      </StepFooter>
    </>
  );
}
