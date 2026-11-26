import type { PlayerView } from "./backend/PlayerBackend";
import { S } from "./strings";

// Bit-perfect status pill, rendered inside the turntable SVG near the POWER lamp.
// It reads the pre-built `format` string the native engine emits on its format
// event (e.g. "FLAC 24/96 → 24/96 ✓", "DSD64 → DoP 176.4 ✓", "→ shared 48k ✗"),
// so the string already carries the ✓/✗; the badge only adds colour.
//
// Hidden for the foobar backend, which never sets `format`. Shows underruns when
// the native engine reports any (> 0). `x`/`y` are the pill centre in SVG units.
export function Badge({ p, x, y }: { p: PlayerView; x: number; y: number }) {
  const text = p.format;
  if (!text) return null; // foobar backend (or nothing playing yet): no badge

  const bitPerfect = p.bitPerfect === true;
  const under = p.underruns ?? 0;
  const label = under > 0 ? `${text}  ⚠${under}` : text;

  // ~5.2 px per char at fontSize 8.5 monospace, plus padding.
  const w = Math.max(96, label.length * 5.2 + 20);
  const fill = bitPerfect ? "#2f6b3a" : "#9c6a1e"; // green = bit-perfect, amber = processed/shared

  return (
    <g pointerEvents="none" role="img" aria-label={label}>
      <title>{bitPerfect ? S.badgeTitleBitPerfect : S.badgeTitleProcessed}</title>
      <rect x={x - w / 2} y={y - 9} width={w} height="18" rx="9" fill={fill} />
      <text
        x={x}
        y={y + 3.4}
        textAnchor="middle"
        fontFamily="'DM Mono', monospace"
        fontSize="8.5"
        letterSpacing=".3"
        fill="#f6f1e6"
      >
        {label}
      </text>
    </g>
  );
}
