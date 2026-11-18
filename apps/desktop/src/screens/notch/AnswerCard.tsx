import { Fragment, useEffect, useRef, useState } from "react";
import { Icon } from "../../components/Icon";
import type { AnswerView } from "../../generated/AnswerView";
import { useTween } from "../../lib/hooks";
import { dispatch } from "../../lib/snapshot";
import { Dots } from "./indicators";
import { STATE_LABEL } from "./status";

export function AnswerCard({ answer }: { answer: AnswerView | null }) {
  if (!answer) return <EmptyAnswer />;
  switch (answer.state) {
    case "preparing":
      return <Preparing answer={answer} />;
    case "failed":
      return <Failed answer={answer} />;
    case "cancelled":
      return <EmptyAnswer />;
    default:
      return <Ready key={answer.id} answer={answer} />;
  }
}

function EmptyAnswer() {
  return (
    <section className="ans idle" aria-label="Answer">
      <p className="ans-empty">Waiting for a question.</p>
    </section>
  );
}

function Preparing({ answer }: { answer: AnswerView }) {
  return (
    <section className="ans preparing" aria-label="Answer" aria-busy="true">
      <div className="ans-top">
        <span className="ans-flag muted">
          <Dots />
          {STATE_LABEL.preparing}
        </span>
      </div>
      <p className="ans-q">{answer.question}</p>
    </section>
  );
}

function Failed({ answer }: { answer: AnswerView }) {
  return (
    <section className="ans failed" aria-label="Answer" role="alert">
      <div className="ans-top">
        <span className="ans-flag">
          <Icon name="alert" size={14} />
          {STATE_LABEL.error}
        </span>
      </div>
      <p className="ans-text">{answer.error ?? "Something went wrong."}</p>
      <div className="ans-actions">
        <button
          type="button"
          className="btn sm"
          onClick={() => dispatch({ type: "ask", text: answer.question, with_screenshot: false })}
        >
          <Icon name="refresh" size={14} />
          Try again
        </button>
      </div>
    </section>
  );
}

function Ready({ answer }: { answer: AnswerView }) {
  const streaming = answer.state === "streaming";
  const latency = useTween(answer.latency_ms ?? 0);
  const words = answer.text.split(/\s+/).filter(Boolean);
  const sentenceEnd = words.findIndex((w) => /[.!?]$/.test(w));
  const boldUntil = sentenceEnd === -1 ? words.length - 1 : sentenceEnd;
  const [copied, setCopied] = useState(false);
  const copiedTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(copiedTimer.current), []);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(answer.text);
      setCopied(true);
      clearTimeout(copiedTimer.current);
      copiedTimer.current = setTimeout(() => setCopied(false), 1600);
    } catch {
      setCopied(false);
    }
  };

  return (
    <section className="ans ready" aria-label="Answer">
      <div className="ans-top">
        <span className="ans-flag">
          <Icon name="bolt" size={14} />
          {STATE_LABEL.ready}
        </span>
        {answer.latency_ms !== null && (
          <span className="lat" title="Time from end of question to first word">
            <b>{latency}</b>
            <span>ms</span>
          </span>
        )}
      </div>
      <p className="ans-text" aria-live="off">
        {words.map((w, i) => (
          // Words only ever append, so the index is a stable identity and each one fades in once.
          // biome-ignore lint/suspicious/noArrayIndexKey: append-only stream
          <Fragment key={i}>
            {i > 0 && " "}
            <span className={i <= boldUntil ? "w b" : "w"}>{w}</span>
          </Fragment>
        ))}
        {streaming && <span className="caret" aria-hidden="true" />}
      </p>
      <div className="ans-actions">
        {streaming ? (
          <button
            type="button"
            className="btn sm"
            onClick={() => dispatch({ type: "cancel_answer" })}
          >
            <Icon name="stop" size={14} />
            Stop
          </button>
        ) : (
          <button type="button" className="btn primary sm" onClick={copy}>
            {copied ? "Copied" : "Use answer"}
          </button>
        )}
      </div>
      <span className="sr" role="status">
        {streaming ? "" : STATE_LABEL.ready}
      </span>
    </section>
  );
}
