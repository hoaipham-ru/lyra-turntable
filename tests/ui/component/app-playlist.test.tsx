// P8.3b — full-app integration (src/App.tsx) on the native backend, with the whole
// Tauri surface mocked. Covers every control a user touches and the 0.4.1 fixes:
//   * playlist card: click / Ctrl-click / Shift-click select, double-click & Enter
//     play, Delete & the row × remove, drag-to-reorder -> playlists_reorder (fix #4)
//   * transport keys (play/pause/next/prev) -> engine_send
//   * volume knob -> endpoint_volume_set (fix #3, end-to-end through the backend)
//   * seek bar -> engine_send seek
//   * File menu (English items), sidebar toggle, playlist selector dropdown
//   * no Vietnamese anywhere in the rendered app (fix #1)
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, within, act } from "@testing-library/react";

type Dto = { title: string; artist: string; album: string; length: number; key: string };

const h = vi.hoisted(() => {
  const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
  const listeners: Record<string, Array<(e: { event: string; payload: unknown }) => void>> = {};
  let settings = { backend: "native", deviceId: "dev1", mode: "exclusive", dsdMode: "auto" };
  const mk = (t: string): Dto => ({ title: t, artist: "", album: "", length: 123, key: t });
  let playlists = [
    { id: "mix", title: "Mixtape", isCurrent: true, itemCount: 3 },
    { id: "def", title: "Default Playlist", isCurrent: false, itemCount: 0 },
  ];
  const items: Record<string, Dto[]> = {
    mix: [mk("Alpha Track"), mk("Bravo Song"), mk("Charlie Tune")],
    def: [],
  };
  let endpointVolume = 0.5;

  const invoke = vi.fn(async (cmd: string, args: Record<string, unknown> = {}) => {
    calls.push({ cmd, args });
    switch (cmd) {
      case "settings_get": return settings;
      case "settings_set": settings = args.settings as typeof settings; return null;
      case "engine_running": return true;
      case "devices": return [];
      case "take_pending": return [];
      case "artwork_read": return null;
      case "playlists_list": return playlists.map((p) => ({ ...p, itemCount: items[p.id]?.length ?? 0 }));
      case "playlists_items": return items[args.id as string] ?? [];
      case "playlists_create": {
        const id = "pl" + (playlists.length + 1);
        playlists = [...playlists, { id, title: args.title as string, isCurrent: false, itemCount: 0 }];
        items[id] = [];
        return id;
      }
      case "playlists_reorder": {
        const a = items[args.id as string];
        const [from, to] = [args.from as number, args.to as number];
        const [moved] = a.splice(from, 1);
        a.splice(to, 0, moved);
        return null;
      }
      case "playlists_remove": {
        const idx = new Set(args.indices as number[]);
        items[args.id as string] = items[args.id as string].filter((_, i) => !idx.has(i));
        return null;
      }
      case "playlists_clear": items[args.id as string] = []; return null;
      case "endpoint_volume_get": return endpointVolume;
      case "endpoint_volume_set": endpointVolume = args.level as number; return null;
      default: return null; // engine_send, engine_shutdown, etc.
    }
  });

  const listen = vi.fn(async (event: string, cb: (e: { event: string; payload: unknown }) => void) => {
    (listeners[event] ||= []).push(cb);
    return () => { listeners[event] = (listeners[event] || []).filter((f) => f !== cb); };
  });
  const emitEngine = (payload: unknown) => {
    for (const cb of listeners["engine://event"] || []) cb({ event: "engine://event", payload });
  };
  const setSize = vi.fn(async () => {});
  const reset = () => {
    calls.length = 0;
    for (const k of Object.keys(listeners)) delete listeners[k];
    settings = { backend: "native", deviceId: "dev1", mode: "exclusive", dsdMode: "auto" };
    playlists = [
      { id: "mix", title: "Mixtape", isCurrent: true, itemCount: 3 },
      { id: "def", title: "Default Playlist", isCurrent: false, itemCount: 0 },
    ];
    items.mix = [mk("Alpha Track"), mk("Bravo Song"), mk("Charlie Tune")];
    items.def = [];
    endpointVolume = 0.5;
    setSize.mockClear();
    try { localStorage.clear(); } catch { /* ignore */ }
    // seed the native-backend choice so readBackendSync picks native
    try { localStorage.setItem("tt.settings", JSON.stringify(settings)); } catch { /* ignore */ }
  };
  return { calls, invoke, listen, emitEngine, setSize, reset };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: h.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: h.listen }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    setSize: h.setSize,
    setAlwaysOnTop: vi.fn(async () => {}),
    minimize: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    startDragging: vi.fn(async () => {}),
  }),
  LogicalSize: class { constructor(public width: number, public height: number) {} },
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: () => Promise.resolve(() => {}) }),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null), save: vi.fn(async () => null) }));
vi.mock("@tauri-apps/plugin-http", () => ({ fetch: vi.fn(async () => { throw new Error("no beefweb"); }) }));

import App from "../../../src/App";

const calledWith = (cmd: string) => h.calls.filter((c) => c.cmd === cmd);
const engineSends = () => calledWith("engine_send").map((c) => (c.args as { cmd?: { type?: string } }).cmd?.type);
// Vietnamese precomposed Latin letters (U+00C0..U+1EF9), excluding the two math
// symbols \u00D7 (U+00D7) and \u00F7 (U+00F7) that the app legitimately uses as glyphs.
const VIET = /[\u00C0-\u00D6\u00D8-\u00F6\u00F8-\u024F\u1E00-\u1EFF]/;

// Stub layout for the playlist rows so the drag-reorder geometry resolves in jsdom.
function layoutRows(rowHeight = 40) {
  const lis = [...document.querySelectorAll(".list > li")] as HTMLElement[];
  lis.forEach((li, i) => {
    li.getBoundingClientRect = () =>
      ({ top: i * rowHeight, bottom: i * rowHeight + rowHeight, height: rowHeight, left: 0, right: 200, width: 200, x: 0, y: i * rowHeight, toJSON() {} }) as DOMRect;
  });
  return lis;
}

async function renderApp() {
  render(<App />);
  // wait for the native backend to connect and the Mixtape to load
  await waitFor(() => expect(document.querySelectorAll(".list > li").length).toBe(3));
  return layoutRows();
}

describe("App on native backend — every control + 0.4.1 fixes", () => {
  beforeEach(() => h.reset());
  afterEach(() => cleanup());

  it("loads the selected playlist's tracks", async () => {
    const lis = await renderApp();
    expect(lis.map((li) => li.querySelector(".t")?.textContent)).toEqual(["Alpha Track", "Bravo Song", "Charlie Tune"]);
  });

  it("click selects a row; Ctrl-click adds; Shift-click extends a range", async () => {
    const lis = await renderApp();
    const row = (i: number) => lis[i].querySelector("button.row") as HTMLElement;
    fireEvent.click(row(0));
    expect(lis[0].getAttribute("data-sel")).toBe("true");
    fireEvent.click(row(2), { ctrlKey: true });
    expect(lis[2].getAttribute("data-sel")).toBe("true");
    expect(lis[0].getAttribute("data-sel")).toBe("true"); // still selected (additive)
    // shift-click from anchor (row 2) back to row 0 selects the whole range
    fireEvent.click(row(0), { shiftKey: true });
    expect(lis.map((li) => li.getAttribute("data-sel"))).toEqual(["true", "true", "true"]);
  });

  it("double-click plays that track (engine_send playAt)", async () => {
    const lis = await renderApp();
    fireEvent.doubleClick(lis[1].querySelector("button.row") as HTMLElement);
    await waitFor(() => expect(engineSends()).toContain("playAt"));
    const playAt = calledWith("engine_send").find((c) => (c.args as { cmd?: { type?: string; index?: number } }).cmd?.type === "playAt");
    expect((playAt!.args as { cmd: { index: number } }).cmd.index).toBe(1);
  });

  it("Enter on a focused row plays it", async () => {
    const lis = await renderApp();
    fireEvent.click(lis[2].querySelector("button.row") as HTMLElement);
    fireEvent.keyDown(lis[2].querySelector("button.row") as HTMLElement, { key: "Enter" });
    await waitFor(() => expect(engineSends()).toContain("playAt"));
  });

  it("Delete removes the selected rows (playlists_remove)", async () => {
    const lis = await renderApp();
    fireEvent.click(lis[1].querySelector("button.row") as HTMLElement);
    fireEvent.keyDown(document.querySelector(".list") as HTMLElement, { key: "Delete" });
    await waitFor(() => expect(calledWith("playlists_remove").length).toBeGreaterThan(0));
    const rm = calledWith("playlists_remove")[0];
    expect((rm.args as { indices: number[] }).indices).toEqual([1]);
    await waitFor(() => expect(document.querySelectorAll(".list > li").length).toBe(2));
  });

  it("the row × button removes a single track", async () => {
    const lis = await renderApp();
    fireEvent.click(lis[0].querySelector("button.rm") as HTMLElement);
    await waitFor(() => expect(calledWith("playlists_remove").length).toBeGreaterThan(0));
    expect((calledWith("playlists_remove")[0].args as { indices: number[] }).indices).toEqual([0]);
  });

  it("drag reorders a track (playlists_reorder) — fix #4", async () => {
    const lis = await renderApp();
    // drag row 2 up to the top
    fireEvent.pointerDown(lis[2], { button: 0, clientY: 2 * 40 + 5, pointerId: 7 });
    fireEvent.pointerMove(document.querySelector(".list") as HTMLElement, { clientY: 5, pointerId: 7 });
    fireEvent.pointerUp(document.querySelector(".list") as HTMLElement, { pointerId: 7 });
    await waitFor(() => expect(calledWith("playlists_reorder").length).toBeGreaterThan(0));
    const r = calledWith("playlists_reorder")[0].args as { id: string; from: number; to: number };
    expect([r.from, r.to]).toEqual([2, 0]);
  });

  it("the volume knob drives the endpoint volume (endpoint_volume_set) — fix #3", async () => {
    await renderApp();
    const knob = screen.getByRole("slider", { name: "Volume" });
    fireEvent.wheel(knob, { deltaY: -100 }); // wheel up
    await waitFor(() => expect(calledWith("endpoint_volume_set").length).toBeGreaterThan(0));
    expect((calledWith("endpoint_volume_set")[0].args as { level: number }).level).toBeGreaterThan(0.5);
  });

  it("transport keys drive the engine (play/pause/next/prev)", async () => {
    await renderApp();
    await act(async () => h.emitEngine({ kind: "state", state: "stopped" }));
    fireEvent.click(screen.getByRole("button", { name: "Play" }));
    await act(async () => h.emitEngine({ kind: "state", state: "playing" }));
    fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.click(screen.getByRole("button", { name: "Previous" }));
    await waitFor(() => {
      const t = engineSends();
      expect(t).toContain("play");
      expect(t).toContain("pause");
      expect(t).toContain("next");
      expect(t).toContain("prev");
    });
  });

  it("the seek bar seeks within the track (engine_send seek)", async () => {
    await renderApp();
    await act(async () => {
      h.emitEngine({ kind: "track", path: "C:/m/a.flac", title: "Alpha Track", durationSeconds: 200 });
      h.emitEngine({ kind: "state", state: "playing" });
    });
    const seek = screen.getByRole("slider", { name: "Position in track" });
    await waitFor(() => expect((seek as HTMLInputElement).disabled).toBe(false));
    fireEvent.change(seek, { target: { value: "500" } });
    await waitFor(() => expect(engineSends()).toContain("seek"));
    const s = calledWith("engine_send").find((c) => (c.args as { cmd?: { type?: string } }).cmd?.type === "seek");
    expect((s!.args as { cmd: { seconds: number } }).cmd.seconds).toBeCloseTo(100, 1);
  });

  it("the File menu opens with English items and no Vietnamese", async () => {
    await renderApp();
    fireEvent.click(screen.getByRole("button", { name: /File/ }));
    const menu = await screen.findByRole("menu");
    const labels = within(menu).getAllByRole("menuitem").map((m) => m.textContent ?? "");
    expect(labels.some((l) => l.includes("Open file"))).toBe(true);
    expect(labels.some((l) => l.includes("New playlist"))).toBe(true);
    expect(labels.some((l) => l.includes("Save playlist"))).toBe(true);
    expect(labels.some((l) => l.includes("Exit"))).toBe(true);
    // native mode hides the foobar2000 preferences item
    expect(labels.some((l) => l.toLowerCase().includes("preferences"))).toBe(false);
    expect(labels.join(" ").match(VIET)).toBeNull();
  });

  it("the playlist selector dropdown switches the shown playlist", async () => {
    await renderApp();
    const dd = document.querySelector(".plhead .dd .dd-btn") as HTMLElement;
    expect(dd.textContent).toContain("Mixtape");
    fireEvent.click(dd);
    const opt = screen.getByRole("option", { name: /Default Playlist/ });
    fireEvent.click(opt);
    await waitFor(() => expect(document.querySelectorAll(".list > li").length).toBe(0)); // Default is empty
  });

  it("the sidebar drawer toggles (window resize requested)", async () => {
    await renderApp();
    const drawer = screen.getByRole("button", { name: /Hide playlist|Show playlist/ });
    expect(drawer.getAttribute("aria-expanded")).toBe("true"); // starts open
    fireEvent.click(drawer);
    await waitFor(() => expect(drawer.getAttribute("aria-expanded")).toBe("false"));
    await waitFor(() => expect(h.setSize).toHaveBeenCalled());
  });

  it("the whole rendered app contains no Vietnamese (fix #1)", async () => {
    await renderApp();
    // open the File menu and Settings so their text is in the scan too
    fireEvent.click(screen.getByRole("button", { name: /File/ }));
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    await screen.findByRole("dialog", { name: "Settings" });
    const text = document.body.textContent ?? "";
    const offenders = [...text].filter((ch) => VIET.test(ch));
    expect(offenders, `Vietnamese chars in UI: ${JSON.stringify(offenders)}`).toEqual([]);
  });
});
