// P8.3b — Dropdown (src/Dropdown.tsx). The 0.4.1 fix: native <select> popups do
// not open in the transparent/undecorated WebView2 window, so settings use an
// app-drawn Dropdown. This proves it opens, lists options, picks by mouse AND
// keyboard, and — critically — that Escape closes ONLY the dropdown and does not
// bubble up to close a surrounding dialog (the Settings panel's Escape handler).
import { describe, it, expect, afterEach, vi } from "vitest";
import { render, screen, fireEvent, cleanup, within } from "@testing-library/react";
import { useState } from "react";
import { Dropdown } from "../../../src/Dropdown";

afterEach(() => cleanup());

const OPTS = [
  { value: "exclusive", label: "WASAPI exclusive (bit-perfect)" },
  { value: "shared", label: "WASAPI shared (not bit-perfect)" },
  { value: "asio", label: "ASIO" },
];

// A controlled host so the picked value is reflected back, like Settings uses it.
function Host({ onChange }: { onChange?: (v: string) => void }) {
  const [v, setV] = useState("exclusive");
  return (
    <Dropdown
      id="m"
      ariaLabel="Mode"
      value={v}
      options={OPTS}
      onChange={(nv) => {
        setV(nv);
        onChange?.(nv);
      }}
    />
  );
}

describe("Dropdown — app-drawn replacement for native <select>", () => {
  it("is closed initially and shows the current value as the button label", () => {
    render(<Host />);
    const btn = screen.getByRole("button", { name: "Mode" });
    expect(btn.getAttribute("aria-expanded")).toBe("false");
    expect(within(btn).getByText("WASAPI exclusive (bit-perfect)")).toBeTruthy();
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("opens on click and lists every option", () => {
    render(<Host />);
    fireEvent.click(screen.getByRole("button", { name: "Mode" }));
    const list = screen.getByRole("listbox");
    expect(list).toBeTruthy();
    const opts = within(list).getAllByRole("option");
    expect(opts.map((o) => o.textContent)).toEqual(OPTS.map((o) => o.label));
    // the current option is marked selected
    expect(within(list).getByRole("option", { selected: true }).textContent).toBe(OPTS[0].label);
  });

  it("picks an option with the mouse and applies it", () => {
    const onChange = vi.fn();
    render(<Host onChange={onChange} />);
    fireEvent.click(screen.getByRole("button", { name: "Mode" }));
    fireEvent.click(screen.getByRole("option", { name: "ASIO" }));
    expect(onChange).toHaveBeenCalledWith("asio");
    // closes and reflects the new value on the button
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(screen.getByRole("button", { name: "Mode" }).textContent).toContain("ASIO");
  });

  it("opens with ArrowDown, moves the highlight, and picks with Enter via the keyboard", () => {
    const onChange = vi.fn();
    render(<Host onChange={onChange} />);
    const btn = screen.getByRole("button", { name: "Mode" });
    btn.focus();
    fireEvent.keyDown(btn, { key: "ArrowDown" }); // opens
    expect(screen.getByRole("listbox")).toBeTruthy();
    fireEvent.keyDown(btn, { key: "ArrowDown" }); // highlight -> shared
    fireEvent.keyDown(btn, { key: "Enter" }); // pick highlighted
    expect(onChange).toHaveBeenCalledWith("shared");
  });

  it("does not fire onChange when the already-selected option is re-picked", () => {
    const onChange = vi.fn();
    render(<Host onChange={onChange} />);
    fireEvent.click(screen.getByRole("button", { name: "Mode" }));
    fireEvent.click(screen.getByRole("option", { name: "WASAPI exclusive (bit-perfect)" }));
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("Escape closes the open dropdown WITHOUT bubbling to a surrounding dialog", () => {
    // Simulate the Settings dialog: a window-level Escape handler that would close
    // the dialog. The Dropdown must stop Escape from reaching it while open.
    const dialogClose = vi.fn();
    const onWinKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") dialogClose();
    };
    window.addEventListener("keydown", onWinKey);
    try {
      render(<Host />);
      fireEvent.click(screen.getByRole("button", { name: "Mode" }));
      const list = screen.getByRole("listbox");
      fireEvent.keyDown(within(list).getAllByRole("option")[0], { key: "Escape" });
      // dropdown closed…
      expect(screen.queryByRole("listbox")).toBeNull();
      // …and the dialog's Escape handler never ran
      expect(dialogClose).not.toHaveBeenCalled();
    } finally {
      window.removeEventListener("keydown", onWinKey);
    }
  });
});
