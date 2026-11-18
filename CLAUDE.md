# Sotto: rules for agents

Read before any change: `docs/design.md`, the ADRs in `docs/adr/`, and the Lessons section of `docs/decisions/LOG.md`. UI work also reads `docs/DESIGN_SYSTEM.md` and `docs/design/ux-rules.md`. Security-relevant work checks `docs/security-checklist.md`.

## Language

Code, comments, docs, commit messages and UI copy are in English.

## Ownership

- `crates/sotto-protocol` is the contract. Change it only when the task says so, keep the shared fixtures in `crates/sotto-protocol/tests/fixtures/sidecar` valid, and regenerate the TypeScript types (`cargo test -p sotto-protocol`), committing `apps/desktop/src/generated`.
- Stay inside the directories your task names. Declare dependencies in your own crate's `Cargo.toml`, not in the root `[workspace.dependencies]`, unless the task says otherwise.

## Rules

- `just check` must pass before you report done. Never weaken a lint, delete a test or add `#[allow]` to get there; fix the cause.
- No `unwrap()` outside tests. Errors use `thiserror` in libraries.
- No secrets, transcript text, prompts or answers in logs.
- Never add a directory to `PATH` to find a binary at runtime, never load code or models from a CDN, never drop data silently under load (see Lessons).
- Comments only for the non-obvious why. No comments narrating the change.
- When you make a call a later reader could get wrong, append it to `docs/decisions/LOG.md` (date, decision, why, when to revisit). Big, hard-to-reverse calls get an ADR.
- When something surprises you (an API that differs from its docs, a tool quirk), add it to the Lessons section of `LOG.md`.

## Tooling

- Rust: rustup from Homebrew; prepend `$(brew --prefix rustup)/bin` to `PATH` in scripts (the `justfile` does).
- The shell is zsh: it does not split unquoted variables.
- Headless Chrome screenshots: minimum layout width is 500 px; use an iframe for narrower shots.
