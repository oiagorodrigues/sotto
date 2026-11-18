# 0002: Rust backend (Axum + Postgres) that never stores meeting content

Status: accepted (2026-11-06)

## Context

The user wants a real backend now, partly to learn it, with accounts, ephemeral provider tokens and room for billing later. They also want to minimise compliance exposure (LGPD and similar) because meetings contain third parties' personal data. A backend that receives audio, transcripts and screenshots would hold exactly that data.

## Decision

- `sotto-api` in Rust with Axum and `sqlx` (`>= 0.8.1`, fixes GHSA-xmrp-424f-vfpx) on Postgres, shipped as a non-root container. Local dev uses Docker Compose with Postgres and Mailpit.
- Responsibilities in v1: accounts (argon2id password, magic link), device login (ADR 0008), opaque access tokens and rotating refresh tokens stored hashed, ephemeral provider tokens (ADR 0005), preference sync (settings, shortcuts, mode names and prompts).
- No endpoint accepts or returns meeting content: no audio, transcripts, screenshots, answers or mode files. Logs never include request bodies.
- Shared request and response types live in `crates/sotto-protocol`.

## Consequences

- The server holds account data and provider master keys only, which keeps its compliance surface small.
- Server-side features that need content (post-meeting summaries with the app closed) are impossible by design until an E2E-encrypted design exists.
- Portable: runs on AWS, Fly or any VPS. Hosting is decided later.

## Alternatives

- Cloudflare Workers with Durable Objects: a natural fit for per-chat agents, but the Agents SDK is TypeScript-only and `workers-rs` locks the backend to Cloudflare.
- AWS serverless: many moving parts and weak local emulation at this scale.
- Laravel (as the open-source "stealth" project planned): no advantage for streaming or shared types.
