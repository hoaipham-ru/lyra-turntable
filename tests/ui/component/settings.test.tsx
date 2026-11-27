// P8.3b — Settings panel (src/Settings.tsx). Verifies the 0.4.1 fixes in situ:
//   * every Settings control is English (no Vietnamese) — fix #1,
//   * every dropdown (device / mode / DSD) is an app-drawn Dropdown that opens,
//     lists options and, when picked, persists via settings_set — fix #2,
//   * the backend radio persists and notifies the app,
//   * Escape closes an open dropdown but NOT the dialog around it.
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, within, act } from "@testing-library/react";

const h = vi.hoisted(() => {
  type S = { backend: string; deviceId: string; mode: string; dsdMode: string };
  const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
  let settings: S = { backend: "foobar", deviceId: "", mode: "exclusive", dsdMode: "auto" };
  const devices = [
    {
      id: "dev-xmos",
      name: "XMOS USB DAC",
      isDefault: true,
      exclusiveFormats: [{ rate: 96000, format: "s24", validBits: 24 }, { rate: 192000, format: "s32", validBits: 24 }],
      dopCapable: true,
      nativeDsdRates: [],
    },
  ];
  const invoke = vi.fn(async (cmd: string, args: Record<string, unknown> = {}) => {
    calls.push({ cmd, args });
    if (cmd === "settings_get") return settings;
    if (cmd === "settings_set") { settings = args.settings as S; return null; }
    if (cmd === "devices") return devices;
    return null;
  });
  const reset = () => {
    calls.length = 0;
    settings = { backend: "foobar", deviceId: "", mode: "exclusive", dsdMode: "auto" };
    try { localStorage.clear(); } catch { /* ignore */ }
  };
  return { calls, invoke, reset };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: h.invoke }));

import { Settings } from "../../../src/Settings";

const lastSet = () => {
  const c = h.calls.filter((x) => x.cmd === "settings_set");
  return c.length ? (c[c.length - 1].args.settings as Record<string, unknown>) : undefined;
};
// Open the app-drawn dropdown that is labelled by the given legend text.
function openDropdownFor(labelText: string) {
  const label = screen.getByText(labelText);
  const field = label.closest("fieldset") as HTMLElement;
  const btn = within(field).getByRole("button");
  fireEvent.click(btn);
  return field;
}

// Vietnamese precomposed Latin letters (U+00C0..U+1EF9), excluding the math
// symbols × (U+00D7) and ÷ (U+00F7) which are legitimate UI glyphs.
const VIET = /[À-ÖØ-öø-ɏḀ-ỿ]/;

describe("Settings panel — English UI, working dropdowns, persistence (0.4.1)", () => {
  beforeEach(() => h.reset());
  afterEach(() => cleanup());

  it("renders an English dialog with English radios and labels", async () => {
    render(<Settings onClose={() => {}} />);
    const dialog = await screen.findByRole("dialog", { name: "Settings" });
    expect(dialog).toBeTruthy();
    expect(within(dialog).getByText("Player backend")).toBeTruthy();
    expect(screen.getByRole("radio", { name: /foobar2000 \(requires foobar2000\)/ })).toBeTruthy();
    expect(screen.getByRole("radio", { name: /Lyra \(bit-perfect\)/ })).toBeTruthy();
    expect(within(dialog).getByText("Output device")).toBeTruthy();
    expect(within(dialog).getByText("Mode")).toBeTruthy();
    expect(within(dialog).getByText("DSD")).toBeTruthy();
  });

  it("contains no Vietnamese text anywhere in the panel", async () => {
    render(<Settings onClose={() => {}} />);
    await screen.findByRole("dialog", { name: "Settings" });
    await waitFor(() => expect(h.calls.some((c) => c.cmd === "devices")).toBe(true));
    // open every dropdown so its option text is included in the scan
    for (const lbl of ["Output device", "Mode", "DSD"]) {
      openDropdownFor(lbl);
    }
    const text = document.querySelector(".dialog")?.textContent ?? "";
    const offenders = [...text].filter((ch) => VIET.test(ch));
    expect(offenders, `Vietnamese chars found: ${JSON.stringify(offenders)} in: ${text}`).toEqual([]);
  });

  it("the backend radio persists via settings_set and notifies the app", async () => {
    const onBackendChange = vi.fn();
    render(<Settings onClose={() => {}} onBackendChange={onBackendChange} />);
    await screen.findByRole("dialog", { name: "Settings" });
    fireEvent.click(screen.getByRole("radio", { name: /Lyra \(bit-perfect\)/ }));
    await waitFor(() => expect(lastSet()?.backend).toBe("native"));
    expect(onBackendChange).toHaveBeenCalledWith("native");
  });

  it("the Mode dropdown opens, lists English options and persists the pick", async () => {
    render(<Settings onClose={() => {}} />);
    await screen.findByRole("dialog", { name: "Settings" });
    const field = openDropdownFor("Mode");
    const opts = within(field).getAllByRole("option").map((o) => o.textContent);
    expect(opts).toEqual(["WASAPI exclusive (bit-perfect)", "WASAPI shared (not bit-perfect)", "ASIO"]);
    fireEvent.click(within(field).getByRole("option", { name: "WASAPI shared (not bit-perfect)" }));
    await waitFor(() => expect(lastSet()?.mode).toBe("shared"));
  });

  it("the DSD dropdown persists the pick", async () => {
    render(<Settings onClose={() => {}} />);
    await screen.findByRole("dialog", { name: "Settings" });
    const field = openDropdownFor("DSD");
    fireEvent.click(within(field).getByRole("option", { name: "PCM (convert)" }));
    await waitFor(() => expect(lastSet()?.dsdMode).toBe("pcm"));
  });

  it("the Output-device dropdown lists probed devices and persists the pick", async () => {
    render(<Settings onClose={() => {}} />);
    await screen.findByRole("dialog", { name: "Settings" });
    await waitFor(() => expect(h.calls.some((c) => c.cmd === "devices")).toBe(true));
    const field = openDropdownFor("Output device");
    // System default + the probed XMOS DAC (with its max exclusive rate)
    const opts = within(field).getAllByRole("option").map((o) => o.textContent);
    expect(opts[0]).toBe("System default");
    expect(opts.some((o) => o?.includes("XMOS USB DAC") && o.includes("192 kHz"))).toBe(true);
    fireEvent.click(within(field).getByRole("option", { name: /XMOS USB DAC/ }));
    await waitFor(() => expect(lastSet()?.deviceId).toBe("dev-xmos"));
  });

  it("Escape closes an open dropdown but does NOT close the dialog; a second Escape does", async () => {
    const onClose = vi.fn();
    render(<Settings onClose={onClose} />);
    await screen.findByRole("dialog", { name: "Settings" });
    const field = openDropdownFor("Mode");
    expect(within(field).queryByRole("listbox")).toBeTruthy();
    // Escape while the dropdown is open: closes the list, dialog stays
    await act(async () => {
      fireEvent.keyDown(within(field).getAllByRole("option")[0], { key: "Escape" });
    });
    expect(within(field).queryByRole("listbox")).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
    // Escape with no dropdown open: closes the dialog
    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
  });
});
