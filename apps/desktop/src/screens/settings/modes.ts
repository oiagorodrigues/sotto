import type { ModeFileView } from "../../generated/ModeFileView";

export const MAX_NAME = 80;
export const MAX_PROMPT = 8_000;

export function tokensText(tokens: number): string {
  if (tokens < 1_000) return `${tokens} tokens`;
  const k = tokens / 1_000;
  return `${k < 10 ? k.toFixed(1) : Math.round(k)}k tokens`;
}

export function filesSummary(count: number, tokens: number): string {
  if (count === 0) return "No files";
  return `${count} ${count === 1 ? "file" : "files"} · ${tokensText(tokens)}`;
}

export function progressText(file: ModeFileView): string {
  if (file.chunks_total === 0) return "Waiting to index";
  return `Indexing ${file.chunks_done} of ${file.chunks_total}`;
}
