import { useState } from "react";
import { Icon } from "../../components/Icon";
import type { TranscriptLine } from "../../generated/TranscriptLine";
import { formatClock } from "../../lib/format";

const VISIBLE_LINES = 4;

export function Transcript({
  lines,
  startedAtUnixMs,
}: {
  lines: TranscriptLine[];
  startedAtUnixMs: number;
}) {
  const visible = lines.slice(-VISIBLE_LINES);
  const [open, setOpen] = useState(true);
  return (
    <section className="tr" aria-label="Transcript">
      <h2>
        <button type="button" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
          <Icon name="chevron" size={12} />
          Transcript
          <span className="mono count">{lines.length}</span>
        </button>
      </h2>
      {!open ? null : visible.length === 0 ? (
        <p className="tr-empty">Nothing heard yet.</p>
      ) : (
        <ol>
          {visible.map((l) => (
            <li
              key={l.id}
              className={`${l.speaker}${l.is_question ? " q" : ""}${l.is_final ? "" : " live"}`}
            >
              <time
                className="mono"
                dateTime={new Date(startedAtUnixMs + l.start_ms).toISOString()}
              >
                {formatClock(startedAtUnixMs + l.start_ms)}
              </time>
              <span className="who">{l.speaker === "me" ? "Me" : "Them"}</span>
              <div>
                {l.is_question && (
                  <span className="qflag">
                    <Icon name="help" size={12} />
                    Question detected
                  </span>
                )}
                <p>{l.text}</p>
              </div>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}
