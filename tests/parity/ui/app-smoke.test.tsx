// P4.6 — UI smoke (mocked native events). lyra-qa / S09.
//
// Proves, without a real engine or audio device, that the alpha app:
//   1. opens the Settings panel,
//   2. can switch the live backend to the native Lyra engine (persists via
//      settings_set and flips the UI to the native backend),
//   3. renders the bit-perfect badge from a mocked `format`/`bitPerfect` event,
//      and updates it from a mocked `underrun` event,
//   4. reflects injected engine://events (state / track / format / position /
//      underrun) in the UI, and
//   5. drives the native engine from the transport buttons via `engine_send`.
//
// The whole Tauri surface (invoke + event listen + window/webview/dialog/http) is
// mocked; NO real IPC, engine or device is touched.
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, act } from "@testing-library/react";

// Shared mock state, hoisted so the vi.mock factories below can close over it and
// the test body can read calls / inject events.
const h = vi.hoisted(() => {
  type Settings = { backend: string; deviceId: string; mode: string; dsdMode: string };
  const listeners: Record<string, Array<(e: { event: string; payload: unknown }) => void>> = {};
  const invokeCalls: Array<{ cmd: string; args: unknown }> = [];
  let settings: Settings = { backend: "foobar", deviceId: "", mode: "exclusive", dsdMode: "auto" };

  const invoke = vi.fn(async (cmd: string, args: Record<string, unknown> = {}) => {
    invokeCalls.push({ cmd, args });
    switch (cmd) {
      case "settings_get":
        return settings;
      case "settings_set":
        settings = args.settings as Settings;
        return null;
      case "engine_running":
        return true;
      case "devices":
        return [];
      case "playlists_list":
        return [{ id: "def", title: "Default Playlist", isCurrent: true, itemCount: 0 }];
      case "playlists_items":
        return [];
      case "playlists_create":
        return "new-pl";
      case "artwork_read":
        return null;
      case "take_pending":
        return [];
      default:
        return null; // engine_send, engine_shutdown, playlists_add/remove/clear, ...
    }
  });

  const listen = vi.fn(async (event: string, cb: (e: { event: string; payload: unknown }) => void) => {
    (listeners[event] ||= []).push(cb);
    return () => {
      listeners[event] = (listeners[event] || []).filter((f) => f !== cb);
    };
  });

  // Push an engine://event payload to every current subscriber (the NativeBackend).
  const emitEngine = (payload: unknown) => {
    for (const cb of listeners["engine://event"] || []) cb({ event: "engine://event", payload });
  };

  const reset = () => {
    invokeCalls.length = 0;
    settings = { backend: "foobar", deviceId: "", mode: "exclusive", dsdMode: "auto" };
    for (const k of Object.keys(listeners)) delete listeners[k];
    try {
      localStorage.clear();
    } catch {
      /* ignore */
    }
  };

  return { listeners, invokeCalls, invoke, listen, emitEngine, reset };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: h.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: h.listen }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    setSize: vi.fn(async () => {}),
    setAlwaysOnTop: vi.fn(async () => {}),
    minimize: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    startDragging: vi.fn(async () => {}),
    show: vi.fn(async () => {}),
    unminimize: vi.fn(async () => {}),
    setFocus: vi.fn(async () => {}),
  }),
  LogicalSize: class {
    constructor(
      public width: number,
      public height: number,
    ) {}
  },
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: (_cb: unknown) => Promise.resolve(() => {}) }),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => null),
  save: vi.fn(async () => null),
}));
// FoobarBackend uses this; make it fail fast so the (unselected) foobar hook just
// reports "not connected" instead of hitting a real network.
vi.mock("@tauri-apps/plugin-http", () => ({
  fetch: vi.fn(async () => {
    throw new Error("no beefweb in UI smoke");
  }),
}));

// Import App only after the mocks + setup (window.__TAURI_INTERNALS__) are in place.
import App from "../../../src/App";

const calledWith = (cmd: string) => h.invokeCalls.filter((c) => c.cmd === cmd);
const lastCmdArg = (cmd: string) => {
  const c = calledWith(cmd);
  return c.length ? (c[c.length - 1].args as Record<string, unknown>) : undefined;
};

// Open Settings and switch the backend radio to native; returns once the UI shows
// the native backend as connected.
async function switchToNative() {
  fireEvent.click(screen.getByRole("button", { name: "Settings" }));
  const dialog = await screen.findByRole("dialog", { name: "Settings" });
  expect(dialog).toBeTruthy();
  const nativeRadio = screen.getByRole("radio", { name: /Lyra \(bit-perfect\)/ });
  await act(async () => {
    fireEvent.click(nativeRadio);
  });
  // close the panel
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
}

describe("P4.6 app UI smoke (native backend, mocked engine)", () => {
  beforeEach(() => {
    h.reset();
  });
  afterEach(() => {
    cleanup();
  });

  it("opens the Settings panel", async () => {
    render(<App />);
    // let the mount effects (settings_get, take_pending) flush
    await waitFor(() => expect(calledWith("settings_get").length).toBeGreaterThan(0));
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const dialog = await screen.findByRole("dialog", { name: "Settings" });
    expect(dialog).toBeTruthy();
    // the backend radios are present
    expect(screen.getByRole("radio", { name: /foobar2000/ })).toBeTruthy();
    expect(screen.getByRole("radio", { name: /Lyra \(bit-perfect\)/ })).toBeTruthy();
  });

  it("switches the live backend to native and persists it via settings_set", async () => {
    render(<App />);
    await waitFor(() => expect(calledWith("settings_get").length).toBeGreaterThan(0));
    await switchToNative();
    // persisted
    const arg = lastCmdArg("settings_set");
    expect((arg?.settings as { backend: string }).backend).toBe("native");
    // the native engine lifecycle was probed
    await waitFor(() => expect(calledWith("engine_running").length).toBeGreaterThan(0));
    // the status pill now reads "Lyra" (native + connected), not foobar2000
    await waitFor(() => expect(document.querySelector(".status")?.textContent).toBe("Lyra"));
  });

  it("renders the bit-perfect badge from a mocked format event and shows underruns", async () => {
    render(<App />);
    await waitFor(() => expect(calledWith("settings_get").length).toBeGreaterThan(0));
    await switchToNative();
    await waitFor(() => expect(calledWith("engine_running").length).toBeGreaterThan(0));

    const badge = "FLAC 24/96 → 24/96 ✓";
    await act(async () => {
      h.emitEngine({ kind: "format", sourceDesc: "FLAC 24/96", deviceDesc: "24/96", bitPerfect: true, badge });
    });
    // Badge renders as role=img with aria-label == the format string.
    await waitFor(() => expect(screen.getByRole("img", { name: badge })).toBeTruthy());

    // an underrun event appends the warning count to the badge label (the accessible
    // name collapses the double space, so match on the normalised form).
    await act(async () => {
      h.emitEngine({ kind: "underrun", count: 3 });
    });
    await waitFor(() => expect(screen.getByRole("img", { name: /✓ ⚠3$/ })).toBeTruthy());
  });

  it("reflects injected state / track / position events in the UI", async () => {
    render(<App />);
    await waitFor(() => expect(calledWith("settings_get").length).toBeGreaterThan(0));
    await switchToNative();
    await waitFor(() => expect(calledWith("engine_running").length).toBeGreaterThan(0));

    await act(async () => {
      h.emitEngine({
        kind: "track",
        path: "C:/music/song.flac",
        title: "Test Song",
        artist: "Tester",
        album: "QA Album",
        durationSeconds: 180,
      });
      h.emitEngine({ kind: "state", state: "playing" });
      h.emitEngine({ kind: "position", frame: 48000, seconds: 12 });
    });

    // the track title shows in the header (it also appears on the vinyl label, so
    // target the header specifically)
    await waitFor(() => expect(document.querySelector("h1.title")?.textContent).toBe("Test Song"));
    // "Now playing" = the "playing" kicker label
    await waitFor(() => expect(screen.getByText("Now playing")).toBeTruthy());
    // position 0:12 shown in the seek row
    await waitFor(() => expect(screen.getByText("0:12")).toBeTruthy());
  });

  it("drives the native engine from the transport buttons via engine_send", async () => {
    render(<App />);
    await waitFor(() => expect(calledWith("settings_get").length).toBeGreaterThan(0));
    await switchToNative();
    await waitFor(() => expect(calledWith("engine_running").length).toBeGreaterThan(0));

    // stopped -> clicking PLAY sends {type:"play"}
    await act(async () => {
      h.emitEngine({ kind: "state", state: "stopped" });
    });
    fireEvent.click(screen.getByRole("button", { name: "Play" }));
    await waitFor(() =>
      expect(calledWith("engine_send").some((c) => (c.args as { cmd?: { type?: string } })?.cmd?.type === "play")).toBe(true),
    );

    // playing -> clicking PAUSE sends {type:"pause"}
    await act(async () => {
      h.emitEngine({ kind: "state", state: "playing" });
    });
    fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    await waitFor(() =>
      expect(calledWith("engine_send").some((c) => (c.args as { cmd?: { type?: string } })?.cmd?.type === "pause")).toBe(true),
    );

    // NEXT / PREV
    fireEvent.click(screen.getByRole("button", { name: "Next" }));
    fireEvent.click(screen.getByRole("button", { name: "Previous" }));
    await waitFor(() => {
      const types = calledWith("engine_send").map((c) => (c.args as { cmd?: { type?: string } })?.cmd?.type);
      expect(types).toContain("next");
      expect(types).toContain("prev");
    });
  });
});
