export interface Shortcut {
  id: string;
  keys: string[];
  label: string;
  hint: string;
  /** Registered by the shell, so it works while another app is in front. */
  global: boolean;
}

export const SHORTCUTS: Shortcut[] = [
  { id: "ask", keys: ["⌘", "↵"], label: "Ask", hint: "Ask", global: true },
  { id: "toggle", keys: ["⌘", "\\"], label: "Expand or collapse", hint: "Collapse", global: true },
  {
    id: "listen",
    keys: ["⌘", "⇧", "L"],
    label: "Start or stop listening",
    hint: "Listen",
    global: true,
  },
  {
    id: "invisible",
    keys: ["⌘", "⇧", "I"],
    label: "Hide from screen shares",
    hint: "Invisible",
    global: true,
  },
  { id: "hide", keys: ["⌘", "⇧", "H"], label: "Hide or show the pill", hint: "Hide", global: true },
  { id: "esc", keys: ["Esc"], label: "Collapse the panel", hint: "Esc", global: false },
];

export const keysText = (s: Shortcut): string => s.keys.join("");
