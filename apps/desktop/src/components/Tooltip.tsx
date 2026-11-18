import {
  cloneElement,
  type ReactElement,
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import "./tooltip.css";

const DELAY_MS = 300;
const GAP = 8;
const EDGE = 8;

interface Props {
  label: string;
  shortcut?: string;
  children: ReactElement<Record<string, unknown>>;
}

interface Placement {
  left: number;
  top: number;
  below: boolean;
}

export function Tooltip({ label, shortcut, children }: Props) {
  const id = useId();
  const anchor = useRef<HTMLElement | null>(null);
  const bubble = useRef<HTMLDivElement | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const [open, setOpen] = useState(false);
  const [placement, setPlacement] = useState<Placement | null>(null);

  const show = useCallback(() => {
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setOpen(true), DELAY_MS);
  }, []);
  const hide = useCallback(() => {
    clearTimeout(timer.current);
    setOpen(false);
    setPlacement(null);
  }, []);

  useEffect(() => () => clearTimeout(timer.current), []);

  useEffect(() => {
    if (!open) return;
    const a = anchor.current?.getBoundingClientRect();
    const b = bubble.current?.getBoundingClientRect();
    if (!a || !b) return;
    const roomAbove = a.top - GAP - b.height >= EDGE;
    const below = !roomAbove;
    const top = below ? a.bottom + GAP : a.top - GAP - b.height;
    const centered = a.left + a.width / 2 - b.width / 2;
    const left = Math.min(Math.max(EDGE, centered), window.innerWidth - b.width - EDGE);
    setPlacement({ left, top, below });
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") hide();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, hide]);

  const childProps = children.props as {
    onMouseEnter?: (e: unknown) => void;
    onMouseLeave?: (e: unknown) => void;
    onFocus?: (e: unknown) => void;
    onBlur?: (e: unknown) => void;
    onClick?: (e: unknown) => void;
  };

  const trigger = cloneElement(children, {
    ref: (node: HTMLElement | null) => {
      anchor.current = node;
    },
    "aria-describedby": open ? id : undefined,
    onMouseEnter: (e: unknown) => {
      childProps.onMouseEnter?.(e);
      show();
    },
    onMouseLeave: (e: unknown) => {
      childProps.onMouseLeave?.(e);
      hide();
    },
    onFocus: (e: unknown) => {
      childProps.onFocus?.(e);
      show();
    },
    onBlur: (e: unknown) => {
      childProps.onBlur?.(e);
      hide();
    },
    onClick: (e: unknown) => {
      childProps.onClick?.(e);
      hide();
    },
  });

  return (
    <>
      {trigger}
      {open &&
        createPortal(
          <div
            ref={bubble}
            id={id}
            role="tooltip"
            className="tooltip"
            data-below={placement?.below ? "" : undefined}
            style={{
              left: placement?.left ?? 0,
              top: placement?.top ?? 0,
              visibility: placement ? "visible" : "hidden",
            }}
          >
            {label}
            {shortcut ? <kbd>{shortcut}</kbd> : null}
          </div>,
          document.body,
        )}
    </>
  );
}
