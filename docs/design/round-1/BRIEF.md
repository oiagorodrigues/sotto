# Round 1 brief: visual directions for Sotto

Sotto is a macOS meeting copilot. It listens to both sides of a call, transcribes them live, detects when the other person asks a question and prepares an answer before they finish, and shows it in an overlay hidden from screen shares. The name comes from *sotto voce*, "in a low voice": discreet, quiet, close to the ear. The look must express that through type, color, spacing and motion, never through literal illustrations (no microphones, sound waves as decoration, musical notes or theatre props).

It must not resemble Cluely or any existing product. No logos or brand references.

## Deliverable per direction

One self-contained HTML file (Google Fonts and CDNs allowed, no build step) that shows the four screens below side by side on a neutral "desktop" backdrop (a soft blurred gradient or a muted photo-free surface that stands in for a video call behind the overlay). Each screen is labelled with a small caption.

1. **Notch pill.** A notched MacBook top edge. The pill in compact state (listening, a tiny live level indicator, the session timer `12:48`) and expanded state just below it, showing "Preparing answer" with the partial question, then the ready answer.
2. **Floating overlay panel** (no notch). Width 380 px in the narrow screenshot and 640 px in the wide one. Contains: a header with session title, mode chip and an "Invisible" indicator; a transcript strip with the last 4 lines (Them / Me labels and `HH:MM` times); the answer card with the streaming answer (show the last words fading in); an input "Ask anything" with a screenshot toggle and the shortcut hint; a small debug chip `answer 640 ms`.
3. **Settings, Models page.** A regular window. Sidebar sections: General, Models, Transcription, Modes, Shortcuts, Privacy, Account. Models page: three providers. Anthropic: key saved, masked `sk-ant-…4f2a`, models "Auto answers: Claude Haiku 5.5", "Manual asks: Claude Sonnet 5.5". OpenAI: not configured, "Add key" action. Ollama: running locally, model `qwen3:8b`, "Free, on this Mac". A spend cap field "Auto-answer spend cap per session" with value `$0.50`. Footer actions on the right: "Cancel", "Save".
4. **Onboarding, step 3 of 4: "Check transcription".** Results table for the bundled test clips: Apple Speech (on device), Portuguese (Brazil): first words 180 ms, final text 420 ms, error rate 7.8%; English (US): 150 ms, 390 ms, 5.1%. Cloud (OpenAI): "Off. Turn on to compare" with a switch and one line explaining that cloud sends meeting audio to the provider. Primary action "Continue", secondary "Run again".

Real content to use:
- Session: "Roadmap sync · Acme". Mode chip: "Sales discovery".
- Transcript: `14:02 Them` "We tried two vendors last year and both broke under load." `14:02 Me` "Got it. What volume are we talking about?" `14:03 Them` "Around forty thousand orders a day, with spikes on Mondays." `14:03 Them` "So how would you handle the migration if their API starts rate limiting us?"
- Answer: "Queue the writes and drain them at the vendor's limit. Batch orders in groups of 500, retry with exponential backoff, and keep a dead-letter queue for anything that fails three times. Monday spikes then become a longer drain, not dropped orders."

## The bar

- Original identity per direction, clean and modern, with soft depth (layered surfaces, subtle borders, refined shadows) and confident type hierarchy.
- Banned: Inter, Roboto, Arial, SF/system stack as the brand font; default Tailwind or shadcn palettes; indigo, violet or purple-pink gradients; uniform `rounded-2xl` everywhere; emoji icons; lorem ipsum; serif fonts.
- Icons: inline SVG, 1.5 px stroke, consistent set (draw them or use Lucide via CDN).
- Motion: animate state changes (pill expand with spring, answer words fading in, level indicator, hover/focus/press). Respect `prefers-reduced-motion`.
- Accessible: AA contrast on translucent surfaces, visible focus, semantic HTML, aria-labels on icon buttons, cursors matching context.
- Follow `docs/design/ux-rules.md`.
- Layout must work in both screenshot widths: 420 px wide (screens stacked vertically, overlay at 380 px) and 1280 px wide (screens in a grid, overlay at 640 px). Nothing overflows horizontally at 420 px.
- Define the direction's tokens as CSS custom properties at the top of the file (color, type scale, spacing, radius, shadow, motion durations and easings), so the chosen one can become the design system.

## Screenshots

Take full-page screenshots with headless Chrome at both widths and look at each one once, fixing anything broken:

```
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu --hide-scrollbars --window-size=420,3200 --screenshot=<out>-420.png file://<html>
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu --hide-scrollbars --window-size=1280,2200 --screenshot=<out>-1280.png file://<html>
```

Adjust the heights so the whole page is captured without large empty areas.
