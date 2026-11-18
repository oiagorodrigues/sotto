import type { TranscriptLine } from "../../generated/TranscriptLine";
import { useNow } from "../../lib/hooks";

const WINDOW_MS = 60_000;

interface Props {
  lines: TranscriptLine[];
  startedAtUnixMs: number;
}

function pct(ms: number, from: number): number {
  return Math.min(100, Math.max(0, ((ms - from) / WINDOW_MS) * 100));
}

export function Timeline({ lines, startedAtUnixMs }: Props) {
  const nowUnix = useNow(250);
  const now = nowUnix - startedAtUnixMs;
  const from = now - WINDOW_MS;
  const question = lines.find((l) => l.is_question);
  const rows = [
    { speaker: "them", label: "Them" },
    { speaker: "me", label: "Me" },
  ] as const;
  const summary = question
    ? "Timeline of the last 60 seconds. A question was detected from them."
    : "Timeline of the last 60 seconds.";

  return (
    <section className="tl" aria-label={summary}>
      <div className="tl-grid">
        {rows.map((row) => (
          <Lane key={row.speaker} label={row.label}>
            {lines
              .filter((l) => l.speaker === row.speaker && l.end_ms >= from)
              .map((l) => {
                const start = pct(l.start_ms, from);
                const end = pct(l.is_final ? l.end_ms : now, from);
                return (
                  <i
                    key={l.id}
                    className={`seg ${row.speaker}${l.is_question ? " q" : ""}`}
                    style={{ left: `${start}%`, width: `${Math.max(0.8, end - start)}%` }}
                  />
                );
              })}
            {row.speaker === "them" && question ? (
              <span
                className="qmark"
                aria-hidden="true"
                style={{ left: `${pct(question.start_ms, from)}%` }}
              />
            ) : null}
          </Lane>
        ))}
        <div className="tl-axis mono" aria-hidden="true">
          <span>-60 s</span>
          <span>-30 s</span>
          <span>now</span>
        </div>
      </div>
    </section>
  );
}

function Lane({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <>
      <span className="tl-lbl">{label}</span>
      <div className={`lane-wrap${label === "Them" ? " t" : ""}`}>
        <div className="lane">{children}</div>
      </div>
    </>
  );
}
