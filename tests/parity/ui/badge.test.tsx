// P4.6 — Badge render logic, table-driven over every corpus format. lyra-qa / S09.
//
// The native engine (src-tauri/src/engine.rs `to_payload`) builds the
// `"{codec} {src} → {dev} {mark}"` badge string and the bitPerfect flag, then
// hands both to the UI; Badge.tsx chooses the colour (green = bit-perfect,
// amber = processed) and appends any underrun count. This test drives Badge.tsx
// directly with one PlayerView per corpus format (and the non-bit-perfect and
// underrun cases) and asserts:
//   - the accessible label is exactly the source→device badge string,
//   - the pill fill is green when bit-perfect and amber when not,
//   - an underrun count is appended, and
//   - the foobar backend (no `format`) renders no badge.
//
// It covers the RENDER half of the badge logic for each format. The STRING-build
// half lives in Rust (`to_payload`) and is cross-checked at integration level by
// p4.ps1's per-spec "Format:" lines; see the P4 report's badge note.
import { describe, it, expect, afterEach } from "vitest";
import { render, cleanup } from "@testing-library/react";
import { Badge } from "../../../src/Badge";
import type { PlayerView } from "../../../src/backend/PlayerBackend";

const GREEN = "#2f6b3a";
const AMBER = "#9c6a1e";

// Minimal PlayerView carrying only what Badge reads.
function view(partial: Partial<PlayerView>): PlayerView {
  return partial as PlayerView;
}

// Renders a Badge inside an <svg> (it returns an SVG <g>) and returns the root.
function renderBadge(p: PlayerView) {
  return render(
    <svg>
      <Badge p={p} x={100} y={50} />
    </svg>,
  );
}

afterEach(() => cleanup());

// One representative per corpus + synthetic format. The badge strings follow the
// engine's documented rule `{CODEC} {bits}/{kHz} → {dev} {mark}` (DSD uses the
// DSD label and the DoP/PCM device descriptor). All are bit-perfect (exclusive,
// device spec == source) except where noted.
const bitPerfectCases: Array<{ name: string; badge: string }> = [
  { name: "FLAC 16/44.1", badge: "FLAC 16/44.1 → 16/44.1 ✓" },
  { name: "FLAC 24/44.1", badge: "FLAC 24/44.1 → 24/44.1 ✓" },
  { name: "FLAC 24/48", badge: "FLAC 24/48 → 24/48 ✓" },
  { name: "FLAC 24/96", badge: "FLAC 24/96 → 24/96 ✓" },
  { name: "FLAC 24/192", badge: "FLAC 24/192 → 24/192 ✓" },
  { name: "WAV 16/44.1", badge: "WAV 16/44.1 → 16/44.1 ✓" },
  { name: "MP3 16/44.1", badge: "MP3 16/44.1 → 16/44.1 ✓" },
  { name: "AIFF 24/96", badge: "AIFF 24/96 → 24/96 ✓" },
  { name: "ALAC 24/96", badge: "ALAC 24/96 → 24/96 ✓" },
  { name: "WAVPACK 24/96", badge: "WAVPACK 24/96 → 24/96 ✓" },
  { name: "DSD64 native", badge: "DFF DSD64 → DSD64 ✓" },
  { name: "DSD64 DoP", badge: "DSF DSD64 → 24/176.4 ✓" },
];

describe("P4.6 badge render — source → device per corpus format", () => {
  it.each(bitPerfectCases)("renders a green bit-perfect badge for $name", ({ badge }) => {
    const { container } = renderBadge(view({ format: badge, bitPerfect: true }));
    const g = container.querySelector("g[role='img']") as SVGGElement;
    expect(g).toBeTruthy();
    // label is exactly the source→device string (carries the ✓ itself)
    expect(g.getAttribute("aria-label")).toBe(badge);
    // the pill fill is green for bit-perfect
    expect(container.querySelector("rect")?.getAttribute("fill")).toBe(GREEN);
    // the ✓ (never ✗) is present in the drawn text
    expect(container.querySelector("text")?.textContent).toContain("✓");
    expect(container.querySelector("text")?.textContent).not.toContain("✗");
  });

  // Lossy / lossless formats that need a tolerance or a rate change can land
  // non-bit-perfect; the badge must then be amber and carry ✗.
  const processedCases: Array<{ name: string; badge: string }> = [
    { name: "FLAC 24/96 resampled to shared 24/48", badge: "FLAC 24/96 → 24/48 ✗" },
    { name: "OPUS to shared 48k", badge: "OPUS 24/48 → 24/48 ✗" },
    { name: "VORBIS to shared 48k", badge: "VORBIS 16/44.1 → 24/48 ✗" },
    { name: "AAC to shared 48k", badge: "AAC 16/44.1 → 24/48 ✗" },
  ];
  it.each(processedCases)("renders an amber processed badge for $name", ({ badge }) => {
    const { container } = renderBadge(view({ format: badge, bitPerfect: false }));
    const g = container.querySelector("g[role='img']") as SVGGElement;
    expect(g.getAttribute("aria-label")).toBe(badge);
    expect(container.querySelector("rect")?.getAttribute("fill")).toBe(AMBER);
    expect(container.querySelector("text")?.textContent).toContain("✗");
  });

  it("appends the underrun count to the label", () => {
    const { container } = renderBadge(view({ format: "FLAC 24/96 → 24/96 ✓", bitPerfect: true, underruns: 2 }));
    const g = container.querySelector("g[role='img']") as SVGGElement;
    expect(g.getAttribute("aria-label")).toContain("⚠2");
    // still green — an underrun does not flip the bit-perfect colour
    expect(container.querySelector("rect")?.getAttribute("fill")).toBe(GREEN);
  });

  it("renders no badge for the foobar backend (no format set)", () => {
    const { container } = renderBadge(view({ format: null }));
    expect(container.querySelector("g[role='img']")).toBeNull();
  });

  it("widens the pill for a longer label (no clipping of long badges)", () => {
    const shortW = Number(
      renderBadge(view({ format: "WAV 16/44.1 → 16/44.1 ✓", bitPerfect: true }))
        .container.querySelector("rect")
        ?.getAttribute("width"),
    );
    cleanup();
    const longW = Number(
      renderBadge(view({ format: "WAVPACK 24/192 → 24/192 ✓ (processed path)", bitPerfect: true }))
        .container.querySelector("rect")
        ?.getAttribute("width"),
    );
    expect(longW).toBeGreaterThan(shortW);
  });
});
