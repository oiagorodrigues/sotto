# Slice 1 review (2026-11-06)

Independent review of `git diff 4b0bdd3..65e5441`. Items are numbered for reference in commits and the decision log. Status is updated as fixes land.

## Act on

1. **Cancelled speculations never count toward spend.** `app/streams.rs` returns on cancel; Anthropic usage is only emitted at the final `message_delta`, so cancelled streams report no `Usage` and the ADR 0006 spend cap can be bypassed. The test `stale_events_from_a_cancelled_stream_are_ignored` injects a synthetic `Usage` and does not test this. Fix: emit input/cache usage at `message_start`, output at the end, and an estimated usage on cancel; test that a cancelled stream counts. Fixed in core: Anthropic usage is emitted at `message_start` and `message_delta` as increments, and a stream cut before its final usage bills an estimate. Test: `a_cancelled_speculation_still_counts_toward_the_cap`.
2. **Spend cap does not apply to unpriced models** (OpenAI entries at 0, unknown models cost 0, table compiled in). Fix: block automatic answers with a visible reason when the auto or classifier model has no positive price; allow a `pricing.toml` override in the app data directory. Fixed in core: Unpriced auto or classifier models block speculation with the notice "Set a price for <model> to use automatic answers" and `last_guard_block`; OpenAI zero prices removed; `<data_dir>/pricing.toml` merges over the bundled table (`AppConfig::data_dir`); Ollama exempt. Tests: `automatic_answers_need_a_price_for_the_model`, `a_price_override_in_the_data_dir_unblocks_a_model`, `an_invalid_price_override_is_reported_and_ignored`.
3. **Core and sidecar clocks differ by the sidecar setup time D.** The sidecar takes t0 before locale resolution, asset install, engine and tap start, then emits `Started`; the core sets t0 on receipt. `TurnEnded` fires D late and latency under-reports by D. Fix (contract already changed): `Started { t0_host_ns, emitted_at_ms }`; the sidecar fills `emitted_at_ms`, the core sets `t0 = now - emitted_at_ms`; tests with a non-zero setup time. Fixed in core: t0 = now - `emitted_at_ms`. Test: `sidecar_setup_time_does_not_delay_the_turn_end` (700 ms setup).
4. **The 4-token classifier yields no text on thinking models.** Haiku 5.5 thinks adaptively when `thinking` is omitted; `gpt-5-mini` spends reasoning tokens from `max_completion_tokens`; Ollama qwen3 models think by default. Fix: classifier sends `thinking: {type: "disabled"}` to Anthropic (allowed at effort high or below) with `max_tokens` 256; OpenAI `reasoning_effort: "minimal"` and a larger budget; Ollama `think: false`; a `max_tokens` stop with no text is an error, not Done. Check answer budgets the same way. Fixed in core: Classifier: thinking disabled, effort low, 256 tokens; OpenAI `reasoning_effort` minimal and a 512 floor; Ollama `think: false`. Answers 1,024 (auto) and 4,096 (manual) tokens; an empty `max_tokens`/`length` stop is an error and a Failed answer. Tests: `classifier_disables_thinking_without_a_budget`, `reasoning_models_get_effort_and_room_for_reasoning`, `classifier_turns_thinking_off`, `max_tokens_with_no_text_is_an_error` (and OpenAI/Ollama equivalents), `an_answer_with_no_text_at_the_token_limit_fails_clearly`.
5. **Sidecar start failures are non-fatal,** so the UI shows Listening/Local with nothing transcribing. Fix: start failures are `fatal: true`; the core sets the engine `Unavailable` and ends the session when `Started` does not arrive within 10 s. Fixed in core: A fatal error or helper exit before `Started` ends the session with engine Unavailable; no `Started` within 10 s does the same and replaces the helper. Tests: `a_fatal_start_error_ends_the_session`, `a_session_ends_when_started_never_arrives`.
6. **Screenshot asks wait 3 s and say "timed out" when screen recording is not granted; nothing requests the permission.** Fix: `PermissionDenied` while a shot is pending falls back at once with a "grant Screen Recording" note; request access on the first screenshot attempt. Fixed in core: `permission_denied` while a shot is pending answers at once with a Screen Recording note; the first screenshot ask of the run sends `RequestPermission { screen_recording }`. Test: `screen_recording_denial_falls_back_at_once`.
7. **Retention purge is never called.** Fix: purge at `App::start` and once a day. Fixed in core: Purge runs when the loop starts and every 24 h (`retention_days`, default 30). Test: `retention_purges_at_start_and_daily`.
8. **Old-session transcripts leak into a new session** after a quick End then Start (finals flushed before `stopped`). Fix: drop transcript events until the new session's `Started`. Fixed in core: Transcript events before the new session's `Started` are dropped. Test: `finals_from_the_previous_session_stay_out_of_the_next_one`.
9. **Cmd+Return focuses the input but the panel never becomes key,** so typing goes to the frontmost app (possibly a meeting chat). Fix: make the panel key on the main thread before emitting the shortcut; check by hand that clicking the input makes it key.

## Consider (accepted)

10. Manual asks consume the automatic-answer cap and guard blocks are silent. Fix: count only auto and classifier spend against the guard; expose the last block reason in `DebugStats`. Fixed in core: Manual spend no longer counts toward the guard; `DebugStats::last_guard_block` added. Tests: `manual_spend_does_not_use_up_the_automatic_answer_cap`, `cooldown_blocks_the_next_automatic_answer`.
11. Keychain read on every stream; ad-hoc dev builds may prompt. Fix: read keys once in the secret worker and cache them; document stable dev signing. Fixed in core: `CachedStore` keeps keys in memory after the first read and follows SetApiKey/RemoveApiKey. Tests: `the_keychain_is_read_once_per_key`, `cached_store_reads_once_and_follows_writes`.
12. Default locale en-US. Fix: system locale when it is in the sidecar's `installed_locales`, else en-US; a locale setting. Fixed in core: System locale when installed, else en-US; saved `locale` setting, `SetLocale` intent, `locale` and `available_locales` in `SettingsView`; `Start` waits for `Ready`. Tests: `the_system_locale_is_used_when_installed`, `an_uninstalled_system_locale_falls_back_to_en_us`, `set_locale_is_checked_saved_and_used_by_the_next_session`, `locale_validation`.
13. No idle timeout on provider streams. Fix: 30 s idle timeout between chunks. Fixed in core: 30 s idle timeout between chunks. Test: `a_stalled_stream_ends_with_an_idle_timeout`.
14. Cache breakpoint on one growing transcript block. Fix: older final lines in their own cached block; skip caching under the model minimum. Fixed in core: Final transcript lines get their own breakpoint; no breakpoint under the 512-token minimum. Tests: `final_transcript_lines_get_their_own_breakpoint`, `short_prompts_are_not_marked_for_caching`, `final_lines_before_the_first_volatile_one_form_the_history`.
15. Debug sidecar path points at a build no recipe makes. Fix: use the `binaries/` copy.

## Noted (accepted)

16. Keychain-failure test assertion cannot fail; make the failing store echo its input. Fixed in core: The failing store echoes its input and the notice redacts the key. Test: `a_keychain_failure_becomes_a_notice_without_the_key`.
17. Screenshot display anchor ignores overlay windows at level 25.
18. App minimum macOS 14 but sidecar needs 26: set `minimumSystemVersion` to 26.0.
19. `SetAutoAnswer` not persisted. Fixed in core: `auto_answer` is saved and read at start. Test: `models_apply_to_new_answers_and_survive_a_restart`.
20. No tracing subscriber installed; add one in the shell that writes to the app's log file, content-free.

## Fixed in sidecar

- Item 3: `Started` carries `emitted_at_ms`, the sidecar clock (ms since t0) read when the event is written, in live and file mode. The Swift type, encoder and fixture tests cover it; file mode on `pt-BR-question.wav` emits `"emitted_at_ms":0`, since setup is instant there.
- Item 5: every failure in `start` (microphone permission, missing fd 3, locale, assets, engine, tap) goes through `Session.failStart`, which tears down what was built and emits `error` with `fatal: true`. The sidecar keeps running. "A session is already running" and "no speakers" stay non-fatal because they leave the running session untouched.
- Item 6: `capture_screenshot` without screen recording access emits `permission_denied` at once (also when ScreenCaptureKit reports `userDeclined`). `request_permission { screen_recording }` already called `CGRequestScreenCaptureAccess()` and then emits `permissions`; unchanged.
- Item 17: `DisplayAnchor` picks the parent's on-screen window at any layer, highest layer first (the level-25 overlay), then front-most; no window falls back to the main display. Unit tested.

## Fixed in shell

- 9: the ask shortcut runs `makeKeyAndOrderFront` on the overlay panel on the main thread before emitting `sotto://shortcut` (`shell.rs`, `macos/mod.rs`). The panel stays non-activating. Still to check by hand: clicking the input makes the panel key.
- 11 (docs side): stable dev signing steps are in `docs/decisions/LOG.md`; no certificate was created.
- 15: debug sidecar path is `src-tauri/binaries/sotto-audio-aarch64-apple-darwin` from `CARGO_MANIFEST_DIR`; `just dev` builds it and runs `pnpm tauri dev`.
- 18: `minimumSystemVersion` is "26.0"; config test added.
- 20: `logging.rs` installs a file subscriber at `<app log dir>/sotto.log`, info level, 5 MB cap with one rotated file.
- 12 (UI side): Settings has a Transcription page (`TranscriptionPage.tsx`) with a language select that dispatches `set_locale`; fixtures and the demo core carry the new fields.
- Core `data_dir` is passed from `real_core.rs` (pricing override).
