# Sotto design system

Chosen in round 1 (2026-11-06, `docs/design/round-1/`): direction 3 "Cue" as the base, with two traits borrowed from the other directions. UX rules live in `docs/design/ux-rules.md` and apply to every screen.

## Idea

Graphite and quiet most of the time. One signal color, the cue, appears only when it is the user's turn: an answer is ready, a question was detected, or the primary action of a screen. The conversation is drawn as two lanes over time (Them above, Me below), so the user sees the detected question on the timeline. Mint marks "listening" and nothing else.

Borrowed:

- From "Margin": the question marker. A thin cue-colored rule and a small "Question detected" flag tie the transcript line to the answer it produced.
- From "Pianissimo": restraint in the narrow overlay. At 380 px it is a single column, and the answer is the largest text on screen, set lighter than the labels.

## Tokens

Defined once as CSS custom properties in `apps/desktop/src/styles/tokens.css`. Contrast ratios below were computed (WCAG 2.x formula), not estimated.

### Color

| Token | Value | Use | Contrast |
|---|---|---|---|
| `--g0` | `#17181A` | window and overlay background | |
| `--g1` | `#1F2023` | raised surface (cards, modules) | |
| `--g2` | `#2A2B2F` | hover, selected rows | |
| `--line` | `#34363A` | dividers and decorative borders only | 1.47:1 on g0 (decorative) |
| `--line-control` | `#6A6D74` | input, select and button borders | 3.43:1 on g0, 3.14:1 on g1 |
| `--text` | `#F2F2F0` | primary text | 15.85:1 on g0 |
| `--text-2` | `#9C9EA3` | secondary text, labels | 6.63:1 on g0, 5.27:1 on g2 |
| `--cue` | `#FF5B2E` | answer ready, question marker, primary button | 5.74:1 on g0, 5.27:1 on g1 |
| `--cue-ink` | `#17181A` | text and icons on a cue background | 5.74:1 on cue |
| `--cue-hover` | `#E87650` | primary button hover | |
| `--cue-wash` | `rgba(255,91,46,.14)` | question highlight on the timeline | |
| `--mint` | `#8FD3C1` | listening state only | 10.37:1 on g0 |
| `--mint-wash` | `rgba(143,211,193,.14)` | listening chip background | |
| `--notch` | `#000000` | notch pill fill, must merge with the hardware | |

Rule: never put `--text` on `--cue` (2.76:1, fails AA). Text on cue is always `--cue-ink`.

### Type

- Text and display: Onest (400, 500, 600, 700), bundled locally, not loaded from Google Fonts at runtime.
- Numbers, times, shortcuts, latency: Red Hat Mono (400, 500, 600), bundled, tabular figures.
- Scale: 11, 12.5, 14, 16, 20, 28 px, plus 34 px for numeric readouts. Tracking -0.025em at 20 px and up.
- The answer: 16 px at 380 px wide, 18 px at 640 px, weight 400, line height 1.5, first sentence 600.

### Space, radius, depth

- Space: 4, 8, 12, 16, 20, 24, 32, 48.
- Radius: panels 6 px, controls 4 px, pills 999 px. The notch pill uses concave "ears" where it meets the top edge.
- Shadows: `--sh-1` `0 1px 0 rgba(0,0,0,.4)`; `--sh-2` `0 1px 2px rgba(0,0,0,.45), 0 4px 10px rgba(0,0,0,.28)`; `--sh-3` `0 2px 4px rgba(0,0,0,.4), 0 14px 28px rgba(0,0,0,.35)`. Depth comes mostly from graphite steps and 1 px borders.

### Motion

- Durations: fast 150 ms, UI 220 ms, large surfaces 380 ms.
- Easing: `--ease-out` `cubic-bezier(.2,.8,.2,1)`; `--ease-spring` `cubic-bezier(.34,1.45,.5,1)` for the notch pill.
- Cue pulse: one pulse when an answer becomes ready, never looping.
- Answer words fade in as they stream; numbers tween; timeline bars grow in real time.
- `prefers-reduced-motion`: no pulse, no spring, no word fade; state changes become instant opacity swaps.

## Components

- **Notch (the single overlay surface).** One `notch` window holds both states; there is no separate overlay window. Black (`--notch`) fill that merges with the hardware notch, with concave ears drawn as SVG paths where it meets the top edge (no ears on displays without a notch, where the same pill sits at the top center).
  - *Compact (the pill).* Height equals the notch height (32 px without a notch); width is the notch width plus two wings of 120 px each (232 px without a notch). The camera area stays empty. Left wing: a mic button (idle grey; listening shows mint level bars, steady while nobody speaks and moving while a line is live; click starts or stops listening). Right wing: the Invisible indicator, a cue dot, "preparing" dots or an alert glyph for the answer in progress, and the session timer while listening. Clicking the pill body expands it.
  - *Expanded.* A panel about 600 px wide (440 to 760, resized from the bottom-right corner and persisted) drops from the pill with the spring easing. The top is `--notch` black at notch height and fades into `--g0` over about 44 px. Top to bottom: bar row (mic with state word, timer, collapse), header (session title or "Not listening", mode chip, Invisible toggle, settings), two-lane timeline (only while listening), answer card (`--g1`, 3 px cue top edge, latency in mono) or the empty state that says how to start, collapsible transcript (last 4 lines, question marker), input bar (screenshot toggle, "Ask anything", mic toggle, send), and a footer of mono shortcut hints with a "?" button that swaps the content for the shortcuts sheet. Height fits the content up to 70% of the screen height, then the body scrolls.
  - *Geometry rule.* The window is always exactly the size of the visible content: the webview measures the surface with a `ResizeObserver` and reports it with `report_notch_size`; the shell clamps it, sizes the panel on the main thread, pins the top edge to the screen top and centers it on the notch. The only transparent margins are the 12 px ears at the sides and 10 px below the expanded panel for its shadow, so nothing the user can see below the pill is covered by an invisible area. The compact state has no shadow and no bottom margin.
  - *Focus.* The panel is non-activating; it becomes key only when the input needs typing (ask shortcut, click in the input).
  - *Tooltips* are not shown in the compact state (the window would clip them); compact controls use `title` and `aria-label`.
- **Provider module.** Bordered `--g1` module with name, status LED and label, masked key (`sk-ant-…4f2a`) with Replace and Remove, model selects. Keys are never revealed.
- **Stepper.** Vertical on the left from 900 px, numbered dots in a top row below that.
- **Results grid.** Mono numerals with a small inline bar per metric.
- **Buttons.** Primary: cue background, cue-ink text. Secondary: transparent, `--line-control` border, `--text`. Icon buttons: 28 px in the notch panel, tooltip and aria-label always.
- **Fields.** `--g0` background, `--line-control` border, cue focus ring of 2 px with 2 px offset.

## Not yet designed

History, Modes editor, Shortcuts recorder, Privacy page, Account and sign-in, error states, the invisible-mode indicator in the notch, and a light theme. They follow the same tokens; a later round can add a light theme if needed.
