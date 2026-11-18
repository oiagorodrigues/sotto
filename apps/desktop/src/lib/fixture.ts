// Scripted session for running the UI outside Tauri. It mirrors `DemoCore` in
// `src-tauri/src/core.rs`: same lines, same timings, same answer.
// Query parameters on the route (`#/notch?at=9000&fail=1&idle=1`) freeze the script for screenshots.

import type { AccountView } from "../generated/AccountView";
import type { AnswerView } from "../generated/AnswerView";
import type { AppSnapshot } from "../generated/AppSnapshot";
import type { CalendarView } from "../generated/CalendarView";
import type { Intent } from "../generated/Intent";
import type { ModeView } from "../generated/ModeView";
import type { OnboardingView } from "../generated/OnboardingView";
import type { SettingsView } from "../generated/SettingsView";
import type { TranscriptLine } from "../generated/TranscriptLine";
import { freezeClock } from "./clock";

const CYCLE_MS = 16_000;
const ELAPSED_AT_START_MS = 768_000;
const QUESTION_END_MS = 4_200;
const STREAM_START_DELAY_MS = 1_200;
const LATENCY_MS = 640;
const WORD_MS = 60;
const QUESTION = "So how would you handle the migration if their API starts rate limiting us?";
const ANSWER =
  "Queue the writes and drain them at the vendor's limit. Batch orders in groups of 500, retry with exponential backoff, and keep a dead-letter queue for anything that fails three times. Monday spikes then become a longer drain, not dropped orders.";

const settings: SettingsView = {
  providers: [
    {
      provider: "anthropic",
      configured: true,
      masked_key: "sk-ant…4f2a",
      models: ["claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5"],
    },
    {
      provider: "openai",
      configured: false,
      masked_key: null,
      models: ["gpt-4o", "gpt-5", "gpt-5-mini"],
    },
    { provider: "ollama", configured: true, masked_key: null, models: ["ollama:qwen3:8b"] },
  ],
  auto_answer_model: "claude-haiku-5-5",
  manual_model: "claude-sonnet-5-5",
  spend_cap_micro_usd: 500_000,
  ollama_running: true,
  locale: "en-US",
  available_locales: ["en-US", "pt-BR"],
  engine_preference: "local",
  cloud_transcription_available: false,
};

const calendar: CalendarView = {
  connection: "connected",
  error: null,
  masked_client_id: "123456….com",
  has_client_secret: true,
  events: [],
  prompt_event_id: null,
};

const account: AccountView = {
  state: "signed_in",
  email: "ana@example.com",
  error: null,
  syncing: false,
  last_synced_ms: Date.now() - 120_000,
  sync_error: null,
};

/** `#/notch?cal=prompt` shows a meeting starting in a minute, `cal=off` an unconfigured calendar. */
const PAGE_LOADED_AT = Date.now();

function calendarView(): CalendarView {
  const at = new URLSearchParams(window.location.hash.split("?")[1] ?? "").get("at");
  const now = at === null ? PAGE_LOADED_AT : 1_700_000_000_000 + Number(at);
  const mode = new URLSearchParams(window.location.hash.split("?")[1] ?? "").get("cal");
  if (mode === "off") {
    return { ...calendar, connection: "not_configured", masked_client_id: null, events: [] };
  }
  if (calendar.connection !== "connected")
    return { ...calendar, events: [], prompt_event_id: null };
  const events = [
    {
      id: "ev-1",
      title: "Roadmap sync with the Acme platform team and finance",
      start_ms: now + 60_000,
      end_ms: now + 1_860_000,
      has_video: true,
    },
    {
      id: "ev-2",
      title: "1:1",
      start_ms: now + 7_200_000,
      end_ms: now + 9_000_000,
      has_video: false,
    },
  ];
  const prompt = mode === "prompt" && !dismissed.has("ev-1") ? "ev-1" : null;
  return { ...calendar, events, prompt_event_id: prompt };
}

const dismissed = new Set<string>();
const onboarding: OnboardingView = {
  consent_acknowledged: false,
  permissions: [
    { permission: "microphone", status: "granted" },
    { permission: "system_audio", status: "not_determined" },
    { permission: "speech_recognition", status: "denied" },
    { permission: "screen_recording", status: "not_determined" },
  ],
  check: {
    status: "not_run",
    clips_done: 0,
    clips_total: 0,
    running_locale: null,
    results: [],
    macos_version: null,
    checked_at_unix_ms: null,
    stale: false,
    error: null,
  },
  model_test: { status: "idle", model: null, latency_ms: null, error: null },
};

const CHECK_RESULTS: OnboardingView["check"]["results"] = [
  {
    locale: "en-US",
    outcome: "measured",
    first_partial_ms: 1072,
    final_ms: 504,
    error_rate_pct: 16,
    message: null,
  },
  {
    locale: "pt-BR",
    outcome: "measured",
    first_partial_ms: 1117,
    final_ms: 331,
    error_rate_pct: 0,
    message: null,
  },
];
const modes: ModeView[] = [
  {
    id: "00000000-0000-0000-0000-00000000a001",
    name: "Sales discovery",
    system_prompt: "You help me run discovery calls. Keep answers under three sentences.",
    files: [
      {
        id: "00000000-0000-0000-0000-00000000f001",
        name: "pricing.md",
        tokens: 2_400,
        status: "ready",
        chunks_done: 0,
        chunks_total: 0,
        error: null,
      },
    ],
    total_tokens: 2_400,
    retrieval: false,
  },
  {
    id: "00000000-0000-0000-0000-00000000a002",
    name: "Support handbook",
    system_prompt: "Answer as a support engineer, citing the handbook.",
    files: [
      {
        id: "00000000-0000-0000-0000-00000000f002",
        name: "handbook.pdf",
        tokens: 182_000,
        status: "ready",
        chunks_done: 0,
        chunks_total: 0,
        error: null,
      },
      {
        id: "00000000-0000-0000-0000-00000000f003",
        name: "runbook.txt",
        tokens: 41_000,
        status: "indexing",
        chunks_done: 38,
        chunks_total: 140,
        error: null,
      },
      {
        id: "00000000-0000-0000-0000-00000000f004",
        name: "old-faq.md",
        tokens: 9_000,
        status: "failed",
        chunks_done: 0,
        chunks_total: 0,
        error: "Ollama is not running",
      },
    ],
    total_tokens: 232_000,
    retrieval: true,
  },
];
let selectedMode: string | null = modes[0]?.id ?? null;

interface Control {
  listening: boolean;
  invisible: boolean;
  autoAnswer: boolean;
  manual: { at: number; question: string } | null;
  cancelled: boolean;
  asks: number;
  fail: boolean;
}

const control: Control = {
  listening: true,
  invisible: true,
  autoAnswer: true,
  manual: null,
  cancelled: false,
  asks: 0,
  fail: false,
};

const words = (text: string, count: number): string =>
  text.split(/\s+/).filter(Boolean).slice(0, count).join(" ");

function line(
  id: string,
  speaker: "me" | "them",
  start: number,
  end: number,
  text: string,
  isFinal: boolean,
): TranscriptLine {
  return {
    id,
    speaker,
    start_ms: ELAPSED_AT_START_MS + start,
    end_ms: ELAPSED_AT_START_MS + end,
    text,
    is_final: isFinal,
    is_question: false,
  };
}

function answerAt(
  dt: number,
  origin: "auto" | "manual",
  question: string,
  id: string,
  lineId: string | null,
): AnswerView {
  const total = ANSWER.split(/\s+/).length;
  const streamed =
    dt < STREAM_START_DELAY_MS ? 0 : Math.floor((dt - STREAM_START_DELAY_MS) / WORD_MS) + 1;
  const base = {
    id,
    origin,
    question,
    model: "claude-haiku-5-5",
    question_line_id: lineId,
  };
  if (control.fail && dt >= STREAM_START_DELAY_MS) {
    return {
      ...base,
      text: "",
      state: "failed",
      latency_ms: null,
      error: "The provider did not answer in time.",
    };
  }
  if (dt < STREAM_START_DELAY_MS) {
    return { ...base, text: "", state: "preparing", latency_ms: null, error: null };
  }
  if (streamed < total) {
    return {
      ...base,
      text: words(ANSWER, streamed),
      state: "streaming",
      latency_ms: LATENCY_MS,
      error: null,
    };
  }
  return { ...base, text: ANSWER, state: "done", latency_ms: LATENCY_MS, error: null };
}

export function scriptSnapshot(t: number, cycleStartUnixMs: number): AppSnapshot {
  const base: Pick<
    AppSnapshot,
    | "invisible"
    | "auto_answer"
    | "engine"
    | "levels"
    | "settings"
    | "calendar"
    | "notice"
    | "onboarding"
    | "modes"
    | "selected_mode_id"
    | "account"
  > = {
    calendar: calendarView(),
    account,
    invisible: control.invisible,
    auto_answer: control.autoAnswer,
    engine: frozenParams().cloud ? "cloud" : "local",
    levels: { me: 0, them: 0 },
    settings,
    notice: frozenParams().notice,
    onboarding,
    modes,
    selected_mode_id: selectedMode,
  };
  const debug = {
    speculations_started: 1,
    speculations_cancelled: 0,
    speculations_revealed: 1,
    spend_micro_usd: 4_200,
    dropped_buffers: 0,
    last_guard_block: null,
    last_latency_ms: null as number | null,
    p50_latency_ms: LATENCY_MS,
    p95_latency_ms: 910,
  };
  if (!control.listening) {
    return { ...base, phase: "idle", session: null, transcript: [], answer: null, debug };
  }
  const transcript: TranscriptLine[] = [
    line(
      "them-1",
      "them",
      -52_000,
      -47_000,
      "We tried two vendors last year and both broke under load.",
      true,
    ),
    line("me-2", "me", -44_000, -41_000, "Got it. What volume are we talking about?", true),
    line(
      "them-3",
      "them",
      -35_000,
      -29_000,
      "Around forty thousand orders a day, with spikes on Mondays.",
      true,
    ),
  ];
  if (t >= 1_000) {
    const total = QUESTION.split(/\s+/).length;
    const done = t >= QUESTION_END_MS;
    const spoken = done
      ? total
      : Math.min(total, Math.max(1, Math.floor(((t - 1_000) * total) / 3_200)));
    const l = line(
      "them-4",
      "them",
      1_000,
      Math.min(t, QUESTION_END_MS),
      words(QUESTION, spoken),
      done,
    );
    l.is_question = done;
    transcript.push(l);
  }
  const auto =
    control.autoAnswer && t >= QUESTION_END_MS
      ? answerAt(t - QUESTION_END_MS, "auto", QUESTION, "auto-answer", "them-4")
      : null;
  const manual = control.manual
    ? answerAt(
        Math.max(0, t - control.manual.at),
        "manual",
        control.manual.question,
        `manual-${control.asks}`,
        null,
      )
    : null;
  const answer = control.cancelled ? null : (manual ?? auto);
  return {
    ...base,
    phase: "listening",
    session: {
      id: "00000000-0000-0000-0000-000000005077",
      title: "Roadmap sync · Acme",
      mode_name: modes.find((m) => m.id === selectedMode)?.name ?? null,
      started_at_unix_ms: cycleStartUnixMs - ELAPSED_AT_START_MS,
    },
    transcript,
    answer,
    levels: { me: 0, them: t >= 1_000 && t < QUESTION_END_MS ? 2 + (Math.floor(t / 150) % 4) : 0 },
    debug: { ...debug, last_latency_ms: answer?.latency_ms ?? null },
  };
}

function frozenParams(): {
  at: number | null;
  fail: boolean;
  notice: string | null;
  idle: boolean;
  check: boolean;
  cloud: boolean;
} {
  const query = window.location.hash.split("?")[1] ?? "";
  const params = new URLSearchParams(query);
  const at = params.get("at");
  return {
    at: at === null ? null : Number(at),
    fail: params.get("fail") === "1",
    notice: params.get("notice"),
    idle: params.get("idle") === "1",
    check: params.get("check") === "done",
    cloud: params.get("cloud") === "1",
  };
}

const listeners = new Set<(s: AppSnapshot) => void>();
let timer: ReturnType<typeof setInterval> | undefined;
let cycleStart = Date.now();
let last: AppSnapshot | null = null;
let idleApplied = false;

function emit(): void {
  const frozen = frozenParams();
  control.fail = frozen.fail;
  if (frozen.idle && !idleApplied) {
    idleApplied = true;
    control.listening = false;
  }
  let snapshot: AppSnapshot;
  if (frozen.at !== null) {
    const start = 1_700_000_000_000;
    freezeClock(start + frozen.at);
    snapshot = scriptSnapshot(frozen.at, start);
  } else {
    const now = Date.now();
    if (now - cycleStart >= CYCLE_MS) {
      cycleStart = now;
      control.cancelled = false;
    }
    snapshot = scriptSnapshot(now - cycleStart, cycleStart);
  }
  if (JSON.stringify(snapshot) === JSON.stringify(last)) return;
  last = snapshot;
  for (const l of listeners) l(snapshot);
}

export function subscribe(listener: (s: AppSnapshot) => void): () => void {
  listeners.add(listener);
  if (frozenParams().check) finishFixtureCheck();
  if (last) listener(last);
  if (timer === undefined) {
    emit();
    timer = setInterval(emit, 50);
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0 && timer !== undefined) {
      clearInterval(timer);
      timer = undefined;
    }
  };
}

const maskOf = (key: string): string => `${key.slice(0, 6)}…${key.slice(-4)}`;

function setMask(id: "anthropic" | "openai" | "ollama", mask: string | null): void {
  const p = settings.providers.find((x) => x.provider === id);
  if (!p || id === "ollama") return;
  p.configured = mask !== null;
  p.masked_key = mask;
}

function finishFixtureCheck(): void {
  const check = onboarding.check;
  check.status = "done";
  check.clips_done = 2;
  check.clips_total = 2;
  check.running_locale = null;
  check.results = CHECK_RESULTS;
  check.macos_version = "27.0 (26A428)";
  check.checked_at_unix_ms = 1_700_000_000_000;
}

function runFixtureCheck(): void {
  const check = onboarding.check;
  check.status = "running";
  check.clips_done = 0;
  check.clips_total = 2;
  check.running_locale = "en-US";
  check.results = [];
  setTimeout(() => {
    finishFixtureCheck();
    onboarding.check.checked_at_unix_ms = Date.now();
    emit();
  }, 1_500);
}

function runFixtureModelTest(): void {
  onboarding.model_test = {
    status: "running",
    model: settings.manual_model,
    latency_ms: null,
    error: null,
  };
  setTimeout(() => {
    onboarding.model_test = {
      status: "passed",
      model: settings.manual_model,
      latency_ms: 612,
      error: null,
    };
    emit();
  }, 800);
}

export async function dispatch(intent: Intent): Promise<void> {
  switch (intent.type) {
    case "start_session":
      control.listening = true;
      break;
    case "end_session":
      control.listening = false;
      break;
    case "ask": {
      if (intent.text.trim() === "") throw new Error("invalid intent: AskEmpty");
      control.asks += 1;
      control.cancelled = false;
      control.manual = { at: Date.now() - cycleStart, question: intent.text };
      break;
    }
    case "cancel_answer":
      control.cancelled = true;
      break;
    case "set_invisible":
      control.invisible = intent.invisible;
      break;
    case "set_auto_answer":
      control.autoAnswer = intent.enabled;
      break;
    case "open_settings":
      window.location.hash = "#/settings";
      break;
    case "acknowledge_consent":
      onboarding.consent_acknowledged = true;
      break;
    case "request_permission": {
      const state = onboarding.permissions.find((p) => p.permission === intent.permission);
      if (state) state.status = "granted";
      break;
    }
    case "run_transcription_check":
      runFixtureCheck();
      break;
    case "test_model":
      runFixtureModelTest();
      break;
    case "reopen_onboarding":
      window.location.hash = "#/onboarding";
      break;
    case "set_api_key":
    case "remove_api_key":
      setMask(intent.provider, intent.type === "set_api_key" ? maskOf(intent.key) : null);
      break;
    case "set_models":
      settings.auto_answer_model = intent.auto_answer_model;
      settings.manual_model = intent.manual_model;
      break;
    case "set_spend_cap":
      settings.spend_cap_micro_usd = intent.micro_usd;
      break;
    case "calendar_set_client":
      calendar.masked_client_id = maskOf(intent.client_id);
      calendar.has_client_secret = intent.client_secret !== null;
      calendar.connection = "disconnected";
      break;
    case "calendar_remove_client":
      calendar.masked_client_id = null;
      calendar.has_client_secret = false;
      calendar.connection = "not_configured";
      break;
    case "calendar_connect":
      calendar.connection = "connected";
      break;
    case "calendar_disconnect":
      calendar.connection = "disconnected";
      break;
    case "calendar_dismiss_prompt":
      dismissed.add(intent.event_id);
      break;
    case "start_calendar_session":
      dismissed.add(intent.event_id);
      control.listening = true;
      break;
    case "create_mode":
      modes.push({
        id: crypto.randomUUID(),
        name: intent.name,
        system_prompt: intent.system_prompt,
        files: [],
        total_tokens: 0,
        retrieval: false,
      });
      break;
    case "update_mode": {
      const m = modes.find((x) => x.id === intent.mode_id);
      if (m) {
        m.name = intent.name;
        m.system_prompt = intent.system_prompt;
      }
      break;
    }
    case "delete_mode": {
      const at = modes.findIndex((x) => x.id === intent.mode_id);
      if (at >= 0) modes.splice(at, 1);
      if (selectedMode === intent.mode_id) selectedMode = null;
      break;
    }
    case "select_mode":
      selectedMode = intent.mode_id;
      break;
    case "pick_mode_files": {
      const m = modes.find((x) => x.id === intent.mode_id);
      if (m) {
        m.files.push({
          id: crypto.randomUUID(),
          name: `notes-${m.files.length + 1}.txt`,
          tokens: 1_200,
          status: "ready",
          chunks_done: 0,
          chunks_total: 0,
          error: null,
        });
        m.total_tokens += 1_200;
      }
      break;
    }
    case "remove_mode_file": {
      const m = modes.find((x) => x.id === intent.mode_id);
      const f = m?.files.find((x) => x.id === intent.file_id);
      if (m && f) {
        m.files = m.files.filter((x) => x.id !== f.id);
        m.total_tokens -= f.tokens;
      }
      break;
    }
  }
  emit();
}
