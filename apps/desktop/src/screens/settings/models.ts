import type { Option } from "../../components/Select";
import type { ProviderId } from "../../generated/ProviderId";
import type { ProviderView } from "../../generated/ProviderView";
import type { SettingsView } from "../../generated/SettingsView";

export const KEY_PREFIX: Record<Exclude<ProviderId, "ollama">, string> = {
  anthropic: "sk-ant-",
  openai: "sk-",
};

const MICRO_PER_USD = 1_000_000;
const MAX_CAP_USD = 50;
const MIN_KEY_CHARS = 20;

export function modelLabel(id: string): string {
  if (id.startsWith("ollama:")) return id.slice("ollama:".length);
  if (id.startsWith("claude-")) {
    const [family = "", ...version] = id.slice("claude-".length).split("-");
    return `Claude ${family.charAt(0).toUpperCase()}${family.slice(1)} ${version.join(".")}`;
  }
  if (id === "gpt-5-mini") return "GPT-5 mini";
  return id.replace(/^gpt-/, "GPT-");
}

export function provider(settings: SettingsView, id: ProviderId): ProviderView | undefined {
  return settings.providers.find((p) => p.provider === id);
}

/** Models of the providers that can answer now, plus the current choice so it never vanishes. */
export function modelOptions(settings: SettingsView, current: string): Option[] {
  const ids = settings.providers.filter((p) => p.configured).flatMap((p) => p.models);
  if (!ids.includes(current)) ids.push(current);
  return ids.map((value) => ({ value, label: modelLabel(value) }));
}

export function capToText(microUsd: number): string {
  return (microUsd / MICRO_PER_USD).toFixed(2);
}

/** Dollars typed by the user to millionths, or null when outside 0 to 50. */
export function parseCap(raw: string): number | null {
  const text = raw.replace(/^\$/, "").trim();
  if (text === "") return null;
  const n = Number(text);
  return Number.isFinite(n) && n >= 0 && n <= MAX_CAP_USD ? Math.round(n * MICRO_PER_USD) : null;
}

export function keyLooksValid(key: string, prefix: string): boolean {
  return (
    key.length >= MIN_KEY_CHARS && key.length <= 300 && !/\s/.test(key) && key.startsWith(prefix)
  );
}
