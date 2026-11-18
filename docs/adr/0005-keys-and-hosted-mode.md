# 0005: BYOK keys in the Keychain, hosted mode through ephemeral tokens

Status: accepted (2026-11-06)

## Context

The user wants bring-your-own-key from day one and a future plan where Sotto pays for AI. Shipping a provider key inside the app leaks it. Ephemeral tokens let the client talk to a provider directly while the server only mints the token, so content does not pass through the backend. Not every provider offers them: OpenAI does for its Realtime API; Deepgram reportedly offers temporary keys (unverified); Anthropic has no equivalent as far as we know.

## Decision

- BYOK: Anthropic, OpenAI and Ollama (local, no key). Keys are stored with `keyring` in the macOS Keychain, read only by the Rust core, never written to disk, logged, synced or sent to the backend.
- Hosted mode: the backend mints short-lived tokens from server-held keys through `POST /v1/realtime-tokens`, rate limited per account and audited without content. v1 offers hosted mode only for cloud transcription (OpenAI Realtime, Deepgram if confirmed).
- Hosted AI answers through Anthropic would need a stateless proxy that sees content. That is out of v1 and needs its own ADR.

## Consequences

- No Sotto-paid AI spend in v1. Users pay their own providers.
- Billing later attaches to hosted mode; the token endpoint is where quotas will apply.

## Alternatives

- Store user keys encrypted on the server and call providers server-side: enables server tasks, but makes the server a custodian of everyone's keys and of meeting content.
- Ship a shared key in the binary: leaks the key (the "stealth" project warns about exactly this).
