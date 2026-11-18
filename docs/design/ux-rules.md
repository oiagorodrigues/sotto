# UX rules

Sotto is a macOS desktop app with two kinds of surfaces: the overlay (the notch pill and the panel that expands from it) that lives on top of a call, and regular windows (settings, onboarding, history). Every screen is checked against these rules.

## Overlay

- The overlay never steals focus unless the user clicks into its input. It never covers the center of the screen by default.
- It reads at a glance: the answer is the largest text; the transcript is secondary; status is a small chip.
- States are explicit and animated: idle, listening, "them" speaking, preparing an answer, answer ready, error. Each state has its own visual signal that does not depend on color alone (shape, motion or icon too).
- Invisible mode is visible to the user: a persistent indicator when the overlay is hidden from capture.
- The expanded panel resizes from the bottom-right corner, 440 to 760 px wide, and fits its content up to 70% of the screen height. Text reflows; nothing truncates mid-word without an ellipsis and a way to expand.
- Notch docking: solid black fill that merges with the hardware notch, compact and expanded states, content kept clear of the camera housing. On screens without a notch the same pill docks to the top center.
- Keyboard first: every overlay action has a shortcut, shown in tooltips and in the footer hints. The window is exactly as large as the visible content, so no invisible area covers what is below it.

## Windows

- Real grid, sidebar navigation for settings from 900 px wide; below that, navigation collapses into a top segmented control or menu. Windows have a minimum width of 560 px.
- Form actions sit on the right: secondary, then primary. Labels above fields, examples as placeholders, inline errors.
- Secrets (API keys) are masked, show the last four characters, and offer replace and remove, never "reveal".
- Destructive actions use icon plus text and a confirmation dialog.

## Shared

- Dialogs follow the WAI-ARIA APG dialog pattern: trap focus, close on Esc, return focus to the trigger.
- Popovers and menus close on outside click and Esc.
- A recognisable action is an icon button with a tooltip and the same aria-label. The lone primary action of a screen keeps its text label.
- Tooltips open after about 300 ms, render in a portal with collision handling, and never hold information that exists nowhere else.
- Custom selects follow the APG select-only combobox (arrows, Enter, Esc, type-ahead).
- Targets at least 24 px; 28 px or more in the overlay.
- Contrast WCAG 2.2 AA, including text over translucent surfaces against both light and dark backdrops. Visible focus rings.
- Motion: 150 to 250 ms for UI, up to 400 ms for large surfaces, spring easing for the notch. Animate transform and opacity only. Respect `prefers-reduced-motion`.
- Cursors match the context: pointer on actions, text on inputs, grab on drag handles, not-allowed on disabled, progress while waiting.
- Copy is short, in English, sentence case. No helper text that the layout can replace.
