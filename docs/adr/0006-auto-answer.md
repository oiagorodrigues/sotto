# 0006: Hidden speculative answers started from partial transcripts

Status: accepted (2026-11-06)

## Context

The must-have feature is answering the other person's questions automatically, with the answer visible almost as soon as they stop talking. A cold answer pays for end-of-turn detection, prompt processing, model reasoning and time to first token in sequence. `SpeechTranscriber` does not punctuate, and volatile results can be revised or revoked.

## Decision

Measurement point. Latency is measured from the end of the last "them" speech of the question turn (the end of the audio time range of the last volatile or final result in the turn, on the sidecar's common clock) to the first answer token rendered in the overlay. Targets: p50 under 1,000 ms and p95 under 2,000 ms, for speculations that started before the turn ended. Cold answers are measured and reported separately.

Pipeline:

1. A lexical heuristic scores every "them" revision: interrogative openers and inversions in pt-BR and English ("como", "por que", "qual", "você poderia", "what", "how", "could you", "tell me about"), request verbs, tag questions ("né?", "right?"), and a question mark only when present. Punctuation is a bonus signal, never required.
2. When the score crosses the threshold, speculation starts immediately: the stable prefix (system prompt, mode context, earlier transcript) marked for prompt caching, plus the partial question. The model reasons while the person is still speaking.
3. The classifier runs in parallel with the speculation, not before it, so it is off the critical path. It uses a local Ollama model only if one is already loaded (warm), otherwise the cheapest configured API model. A "not a question" verdict cancels the speculation.
4. End of turn: the "them" volatile text has been stable for a window (default 400 ms with no new text). Do not wait for a final result: in the latency spike a mid-stream final arrived about 1 s after its sentence ended, while partials kept pace with the audio. On end of turn the sidecar calls `finalize(through:)` to pull the final early. The stable text is compared with the speculated question; if the meaning changed (token overlap below a threshold, or the classifier disagrees), cancel and restart with the new text.
5. The answer is revealed at end of turn, and only after the classifier has confirmed a question (the spike showed transcription errors can erase lexical cues, so the heuristic alone is not trusted). From step 2 on the overlay shows a "preparing" state so the user knows it is coming.
6. "Me" speech never cancels a speculation. The detector only reads the "them" stream, so echo leaking into the mic cannot trigger a duplicate.

Guards: one live speculation at a time; a cooldown between automatic answers (default 8 s); a cap on speculative starts per minute (default 6); a spend cap per session in dollars (default $0.50) computed from token usage and a dated price table in config; automatic answers can be switched off. Counters and estimated spend are shown in the debug panel and settings.

Defaults (user-editable): automatic answers and classifier on `claude-haiku-5-5` at low effort, manual asks on `claude-sonnet-5-5`, Ollama as the free option. Prices from the API docs cached on 2026-10-06: Haiku 5.5 $0.10 / $0.50, Sonnet 5.5 $2 / $10 per million input / output tokens.

## Consequences

- The latency win comes from overlapping reasoning and prefix processing with the question itself, and from caching the prefix. It costs wasted tokens on cancelled speculations, which the guards bound.
- Prompt caching only applies above a model-dependent minimum prefix size (512 to 4,096 tokens), so short prefixes run uncached.
- Thresholds are provisional. The fixture set is synthetic at first (labelled snippets written for pt-BR and English, including revision sequences) and gets recalibrated with real meetings.

## Alternatives

- Stream the answer visibly while the question is still being asked: fastest, but shows answers to questions that change mid-sentence.
- Wait for a confirmed final question, then answer cold: simplest, the latency of a manual ask plus the model's reasoning time.
- Classifier before speculation: cheaper, but adds its full round trip to the critical path.
