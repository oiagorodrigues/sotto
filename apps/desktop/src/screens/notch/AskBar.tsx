import { useCallback, useEffect, useRef, useState } from "react";
import { Icon } from "../../components/Icon";
import { IconButton } from "../../components/IconButton";
import type { AppSnapshot } from "../../generated/AppSnapshot";
import { dispatch } from "../../lib/snapshot";
import { isListening, toggleListening } from "./CompactBar";

interface Props {
  snapshot: AppSnapshot;
  /** Bumped by the ask shortcut once the panel is open. */
  askTick: number;
}

export function AskBar({ snapshot, askTick }: Props) {
  const input = useRef<HTMLInputElement | null>(null);
  const handledTick = useRef(askTick);
  const [text, setText] = useState("");
  const [screenshot, setScreenshot] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const listening = isListening(snapshot);

  const submit = useCallback(async () => {
    const value = text.trim();
    if (value === "") return;
    try {
      await dispatch({ type: "ask", text: value, with_screenshot: screenshot });
      setText("");
      setError(null);
    } catch (e) {
      setError(typeof e === "string" ? e : "Could not send the question.");
    }
  }, [text, screenshot]);

  useEffect(() => {
    if (askTick === handledTick.current) return;
    handledTick.current = askTick;
    if (document.activeElement === input.current && text.trim() !== "") void submit();
    else input.current?.focus();
  }, [askTick, text, submit]);

  return (
    <form
      className="askbar"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <IconButton
        icon="camera"
        label="Include a screenshot"
        aria-pressed={screenshot}
        onClick={() => setScreenshot((v) => !v)}
      />
      <div className="ask">
        <input
          ref={input}
          className="input"
          value={text}
          placeholder="Ask anything"
          aria-label="Ask anything"
          aria-invalid={error ? true : undefined}
          maxLength={4000}
          onChange={(e) => setText(e.target.value)}
        />
        <kbd>⌘↵</kbd>
      </div>
      <IconButton
        icon="mic"
        label={listening ? "Stop listening" : "Start listening"}
        shortcut="⌘⇧L"
        aria-pressed={listening}
        className={listening ? "icon-btn mic-toggle on" : "icon-btn mic-toggle"}
        disabled={snapshot.phase === "onboarding"}
        onClick={() => void toggleListening(snapshot)}
      />
      <IconButton
        icon="corner"
        label="Send"
        shortcut="↵"
        type="submit"
        className="icon-btn send"
        disabled={text.trim() === ""}
      />
      {error ? (
        <p className="field-error" role="alert">
          <Icon name="alert" size={12} />
          {error}
        </p>
      ) : null}
    </form>
  );
}
