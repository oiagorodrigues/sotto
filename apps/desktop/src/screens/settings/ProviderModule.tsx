import { useState } from "react";
import { Dialog } from "../../components/Dialog";
import { Icon } from "../../components/Icon";
import type { ProviderView } from "../../generated/ProviderView";
import { dispatch } from "../../lib/snapshot";
import { KEY_PREFIX, keyLooksValid } from "./models";

interface Props {
  id: "anthropic" | "openai";
  name: string;
  view: ProviderView | undefined;
}

export function ProviderModule({ id, name, view }: Props) {
  const [dialog, setDialog] = useState<"key" | "remove" | null>(null);
  const saved = view?.configured === true;
  const prefix = KEY_PREFIX[id];

  return (
    <section className="mod" aria-labelledby={`${id}-name`}>
      <div className="mod-h">
        <h3 id={`${id}-name`}>{name}</h3>
        <span className="status">
          <span className={saved ? "led on" : "led"} aria-hidden="true" />
          {saved ? "Key saved" : "Not configured"}
        </span>
      </div>
      <div className="mod-b">
        <div className="field">
          <span className="lbl" id={`${id}-key-label`}>
            API key
          </span>
          <div className="keybox">
            <Icon name="key" />
            <span className="mono" id={`${id}-key-value`}>
              {view?.masked_key ?? `${prefix}…`}
            </span>
          </div>
        </div>
        <div className="row">
          {saved ? (
            <>
              <button type="button" className="btn sm" onClick={() => setDialog("key")}>
                <Icon name="swap" size={14} />
                Replace
              </button>
              <button type="button" className="btn sm" onClick={() => setDialog("remove")}>
                <Icon name="trash" size={14} />
                Remove
              </button>
            </>
          ) : (
            <button type="button" className="btn primary sm" onClick={() => setDialog("key")}>
              Add key
            </button>
          )}
        </div>
      </div>
      {dialog === "key" && (
        <KeyDialog
          name={name}
          prefix={prefix}
          replacing={saved}
          onClose={() => setDialog(null)}
          onSave={(key) => dispatch({ type: "set_api_key", provider: id, key })}
        />
      )}
      {dialog === "remove" && (
        <Dialog
          title={`Remove the ${name} key?`}
          onClose={() => setDialog(null)}
          footer={
            <>
              <button type="button" className="btn" data-autofocus onClick={() => setDialog(null)}>
                Cancel
              </button>
              <button
                type="button"
                className="btn danger"
                onClick={() => {
                  void dispatch({ type: "remove_api_key", provider: id });
                  setDialog(null);
                }}
              >
                <Icon name="trash" size={14} />
                Remove key
              </button>
            </>
          }
        >
          <p>Answers from {name} stop until you add a key again.</p>
        </Dialog>
      )}
    </section>
  );
}

function KeyDialog({
  name,
  prefix,
  replacing,
  onClose,
  onSave,
}: {
  name: string;
  prefix: string;
  replacing: boolean;
  onClose: () => void;
  onSave: (key: string) => Promise<void>;
}) {
  const [value, setValue] = useState("");
  const [touched, setTouched] = useState(false);
  const [rejected, setRejected] = useState(false);
  const valid = keyLooksValid(value.trim(), prefix);
  const submit = () => {
    setTouched(true);
    if (!valid) return;
    const key = value.trim();
    // The key leaves this component's state before the intent is even settled.
    setValue("");
    onSave(key).then(onClose, () => setRejected(true));
  };
  return (
    <Dialog
      title={replacing ? `Replace the ${name} key` : `Add a ${name} key`}
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="button" className="btn primary" onClick={submit}>
            Save key
          </button>
        </>
      }
    >
      <form
        className="field"
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <label htmlFor="key-input">API key</label>
        <input
          id="key-input"
          data-autofocus
          className="input mono"
          type="password"
          autoComplete="off"
          spellCheck={false}
          placeholder={`${prefix}…`}
          value={value}
          aria-invalid={(touched && !valid) || rejected ? true : undefined}
          aria-describedby="key-error"
          onChange={(e) => {
            setRejected(false);
            setValue(e.target.value);
          }}
        />
        {(touched && !valid) || rejected ? (
          <p className="field-error" id="key-error" role="alert">
            <Icon name="alert" size={12} />
            {rejected
              ? "Sotto could not save that key. Paste it again."
              : `The key should start with ${prefix} and be at least 20 characters.`}
          </p>
        ) : null}
      </form>
    </Dialog>
  );
}
