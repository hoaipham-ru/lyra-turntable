// P8.3b — Turntable volume knob (src/Turntable.tsx). The 0.4.1 fix wired the knob
// to the endpoint volume (p.setVolume -> endpoint_volume_set); 0.4.0 shipped it as
// a no-op. This proves drag / wheel / arrow keys all call p.setVolume with a
// sensible value, that the aria-valuenow reflects the level, and that in a forced
// bit-perfect lock the knob is a no-op and marked disabled.
import { describe, it, expect, afterEach, vi } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { Turntable } from "../../../src/Turntable";
import type { PlayerView } from "../../../src/backend/PlayerBackend";

afterEach(() => cleanup());

// A minimal live PlayerView: stopped, no track, volume in the middle. setVolume is
// a spy so we can assert the knob drives it.
function view(partial: Partial<PlayerView> = {}): PlayerView {
  return {
    connected: true,
    status: "",
    state: "stopped",
    track: null,
    position: 0,
    duration: 0,
    volume: 0.5,
    art: null,
    playlists: [],
    selected: null,
    list: [],
    current: -1,
    playPause: vi.fn(),
    next: vi.fn(),
    prev: vi.fn(),
    seek: vi.fn(),
    setVolume: vi.fn(),
    playAt: vi.fn(),
    selectPlaylist: vi.fn(async () => {}),
    newPlaylist: vi.fn(async () => null),
    removeItems: vi.fn(async () => {}),
    clearPlaylist: vi.fn(async () => {}),
    refresh: vi.fn(async () => {}),
    waitForItems: vi.fn(async () => true),
    ensureDefault: vi.fn(async () => null),
    clearById: vi.fn(async () => {}),
    playIn: vi.fn(),
    playingPlaylist: null,
    ...partial,
  };
}

const knob = () => screen.getByRole("slider", { name: "Volume" });

describe("Turntable volume knob — drives endpoint volume (0.4.1 fix)", () => {
  it("reflects the current level in aria-valuenow", () => {
    render(<Turntable p={view({ volume: 0.5 })} />);
    expect(knob().getAttribute("aria-valuenow")).toBe("50");
    expect(knob().getAttribute("aria-disabled")).toBe("false");
  });

  it("wheel up raises the volume via setVolume", () => {
    const setVolume = vi.fn();
    render(<Turntable p={view({ volume: 0.5, setVolume })} />);
    fireEvent.wheel(knob(), { deltaY: -100 }); // wheel up
    expect(setVolume).toHaveBeenCalledTimes(1);
    expect(setVolume.mock.calls[0][0]).toBeCloseTo(0.54, 5);
  });

  it("wheel down lowers the volume via setVolume", () => {
    const setVolume = vi.fn();
    render(<Turntable p={view({ volume: 0.5, setVolume })} />);
    fireEvent.wheel(knob(), { deltaY: 100 }); // wheel down
    expect(setVolume.mock.calls[0][0]).toBeCloseTo(0.46, 5);
  });

  it("ArrowUp / ArrowDown step the volume via setVolume", () => {
    const setVolume = vi.fn();
    render(<Turntable p={view({ volume: 0.5, setVolume })} />);
    fireEvent.keyDown(knob(), { key: "ArrowUp" });
    expect(setVolume.mock.calls[0][0]).toBeCloseTo(0.55, 5);
    fireEvent.keyDown(knob(), { key: "ArrowDown" });
    expect(setVolume.mock.calls[1][0]).toBeCloseTo(0.45, 5);
  });

  it("drag up increases the volume via setVolume", () => {
    const setVolume = vi.fn();
    render(<Turntable p={view({ volume: 0.5, setVolume })} />);
    const k = knob();
    fireEvent.pointerDown(k, { clientY: 300, pointerId: 1, button: 0 });
    fireEvent.pointerMove(k, { clientY: 260, pointerId: 1 }); // 40px up -> +0.25
    expect(setVolume).toHaveBeenCalled();
    const v = setVolume.mock.calls[setVolume.mock.calls.length - 1][0];
    expect(v).toBeGreaterThan(0.5);
    expect(v).toBeCloseTo(0.75, 2);
    fireEvent.pointerUp(k, { pointerId: 1 });
  });

  it("clamps to the 0..1 range", () => {
    const setVolume = vi.fn();
    render(<Turntable p={view({ volume: 0.98, setVolume })} />);
    fireEvent.wheel(knob(), { deltaY: -100 });
    expect(setVolume.mock.calls[0][0]).toBe(1);
  });

  it("is a no-op and marked disabled when the bit-perfect lock is forced", () => {
    const setVolume = vi.fn();
    render(<Turntable p={view({ setVolume })} volumeLocked />);
    const k = knob();
    expect(k.getAttribute("aria-disabled")).toBe("true");
    fireEvent.wheel(k, { deltaY: -100 });
    fireEvent.keyDown(k, { key: "ArrowUp" });
    fireEvent.pointerDown(k, { clientY: 300, pointerId: 1, button: 0 });
    fireEvent.pointerMove(k, { clientY: 200, pointerId: 1 });
    expect(setVolume).not.toHaveBeenCalled();
  });
});
