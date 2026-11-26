import { useEffect, useRef, useState, type KeyboardEvent } from "react";

// Drop-in replacement for <select>. Native <select> popups do not open in this
// window (transparent, undecorated WebView2), so the list is drawn by the app,
// like the File menu. Keyboard: Enter/Space/Alt+Down open, arrows move, Enter
// picks, Escape closes (without closing the dialog around it).
export type Option<T extends string> = { value: T; label: string };

export function Dropdown<T extends string>({
  id,
  value,
  options,
  onChange,
  ariaLabel,
  className,
}: {
  id?: string;
  value: T;
  options: Option<T>[];
  onChange: (v: T) => void;
  ariaLabel?: string;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const [hi, setHi] = useState(0);
  const wrap = useRef<HTMLDivElement>(null);
  const cur = options.find((o) => o.value === value);

  useEffect(() => {
    if (!open) return;
    setHi(Math.max(0, options.findIndex((o) => o.value === value)));
    const away = (e: PointerEvent) => {
      if (!wrap.current?.contains(e.target as Node)) setOpen(false);
    };
    addEventListener("pointerdown", away, true);
    return () => removeEventListener("pointerdown", away, true);
  }, [open]);

  const pick = (i: number) => {
    const o = options[i];
    setOpen(false);
    if (o && o.value !== value) onChange(o.value);
  };

  const onKey = (e: KeyboardEvent) => {
    if (!open) {
      if (e.key === "Enter" || e.key === " " || e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        e.stopPropagation();
        setOpen(true);
      }
      return;
    }
    e.stopPropagation();
    if (e.key === "Escape") { e.preventDefault(); setOpen(false); }
    else if (e.key === "ArrowDown") { e.preventDefault(); setHi((h) => Math.min(options.length - 1, h + 1)); }
    else if (e.key === "ArrowUp") { e.preventDefault(); setHi((h) => Math.max(0, h - 1)); }
    else if (e.key === "Enter" || e.key === " ") { e.preventDefault(); pick(hi); }
    else if (e.key === "Tab") setOpen(false);
  };

  return (
    <div className={"dd " + (className ?? "") + (open ? " open" : "")} ref={wrap} onKeyDown={onKey}>
      <button
        type="button"
        id={id}
        className="dd-btn"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={ariaLabel}
        onClick={() => setOpen((o) => !o)}
      >
        <span className="dd-val">{cur?.label ?? ""}</span>
        <span className="dd-caret" aria-hidden>▾</span>
      </button>
      {open && (
        <ul className="dd-list" role="listbox" aria-labelledby={id}>
          {options.map((o, i) => (
            <li
              key={o.value}
              role="option"
              aria-selected={o.value === value}
              className={(i === hi ? "hi " : "") + (o.value === value ? "sel" : "")}
              onPointerEnter={() => setHi(i)}
              onClick={() => pick(i)}
            >
              {o.label}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
