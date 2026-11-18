// biome-ignore-all lint/a11y/useSemanticElements: the APG select-only combobox is a div with role combobox
// biome-ignore-all lint/a11y/noNoninteractiveElementToInteractiveRole: listbox options are driven by aria-activedescendant on the combobox
// biome-ignore-all lint/a11y/useFocusableInteractive: focus stays on the combobox, options are never focused
// biome-ignore-all lint/a11y/useKeyWithClickEvents: keyboard input is handled on the combobox
import { useEffect, useId, useRef, useState } from "react";
import { Icon } from "./Icon";
import "./select.css";

export interface Option {
  value: string;
  label: string;
}

interface Props {
  label: string;
  options: Option[];
  value: string;
  onChange: (value: string) => void;
  disabled?: boolean;
  placeholder?: string;
  /** A compact chip for toolbars; the label stays for screen readers. */
  chip?: boolean;
}

/** WAI-ARIA APG select-only combobox: arrows, Home/End, Enter, Space, Escape and type-ahead. */
export function Select({ label, options, value, onChange, disabled, placeholder, chip }: Props) {
  const uid = useId();
  const labelId = `${uid}-label`;
  const listId = `${uid}-list`;
  const root = useRef<HTMLDivElement | null>(null);
  const typed = useRef({ text: "", at: 0 });
  const selectedIndex = Math.max(
    0,
    options.findIndex((o) => o.value === value),
  );
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(selectedIndex);
  const selected = options.find((o) => o.value === value);

  useEffect(() => {
    if (!open) return;
    const onPointer = (e: PointerEvent) => {
      if (!root.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", onPointer);
    return () => document.removeEventListener("pointerdown", onPointer);
  }, [open]);

  const openList = () => {
    setActive(selectedIndex);
    setOpen(true);
  };
  const choose = (index: number) => {
    const o = options[index];
    if (o) onChange(o.value);
    setOpen(false);
  };
  const move = (next: number) => setActive(Math.min(options.length - 1, Math.max(0, next)));

  const typeAhead = (char: string) => {
    const now = Date.now();
    const state = typed.current;
    state.text = now - state.at > 600 ? char : state.text + char;
    state.at = now;
    const needle = state.text.toLowerCase();
    const from = state.text.length === 1 ? active + 1 : active;
    const ordered = [...options.keys()].map((i) => (from + i) % options.length);
    const hit = ordered.find((i) => options[i]?.label.toLowerCase().startsWith(needle));
    if (hit === undefined) return;
    if (open) setActive(hit);
    else onChange(options[hit]?.value ?? value);
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (disabled) return;
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        if (!open) openList();
        else move(active + 1);
        break;
      case "ArrowUp":
        e.preventDefault();
        if (!open) openList();
        else move(active - 1);
        break;
      case "Home":
        e.preventDefault();
        if (open) move(0);
        break;
      case "End":
        e.preventDefault();
        if (open) move(options.length - 1);
        break;
      case "Enter":
      case " ":
        e.preventDefault();
        if (open) choose(active);
        else openList();
        break;
      case "Escape":
        if (open) {
          e.preventDefault();
          e.stopPropagation();
          setOpen(false);
        }
        break;
      case "Tab":
        if (open) choose(active);
        break;
      default:
        if (e.key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey) typeAhead(e.key);
    }
  };

  return (
    <div className={chip ? "field select-chip" : "field"} ref={root}>
      <span className={chip ? "sr" : "lbl"} id={labelId}>
        {label}
      </span>
      <div className="select-wrap">
        <div
          className="select input"
          role="combobox"
          tabIndex={disabled ? -1 : 0}
          aria-labelledby={labelId}
          aria-haspopup="listbox"
          aria-expanded={open}
          aria-controls={listId}
          aria-disabled={disabled || undefined}
          aria-activedescendant={open ? `${uid}-opt-${active}` : undefined}
          onClick={() => !disabled && (open ? setOpen(false) : openList())}
          onKeyDown={onKeyDown}
          onBlur={() => setOpen(false)}
        >
          <span className={selected ? undefined : "placeholder"}>
            {selected?.label ?? placeholder ?? ""}
          </span>
          <Icon name="chevron" size={12} />
        </div>
        {open && (
          <ul id={listId} role="listbox" aria-labelledby={labelId} className="listbox">
            {options.map((o, i) => (
              <li
                key={o.value}
                id={`${uid}-opt-${i}`}
                role="option"
                aria-selected={o.value === value}
                data-active={i === active || undefined}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => choose(i)}
                onMouseMove={() => setActive(i)}
              >
                {o.label}
                {o.value === value && <Icon name="check" size={14} />}
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
