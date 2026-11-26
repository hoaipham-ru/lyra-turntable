import { useRef } from "react";
import type { PlayerView } from "./backend/PlayerBackend";
import { Badge } from "./Badge";
import { S } from "./strings";

// Geometry (SVG units). Original design: cream aluminium plinth, smoked lid hinges, straight arm.
const W = 640, H = 500;
const C = { x: 250, y: 262 };          // platter centre
const R_PLATTER = 196, R_VINYL = 184, R_LABEL = 60;
const R_START = 176, R_END = 74;       // first / last groove
const PIV = { x: 548, y: 96 };         // tonearm pivot
const L = 318;                         // pivot -> stylus
const REST = (101 * Math.PI) / 180;    // parked angle

// stylus angle for a groove radius: intersection of circle(PIV, L) and circle(C, r)
function armAngle(r: number) {
  const dx = C.x - PIV.x, dy = C.y - PIV.y, d = Math.hypot(dx, dy);
  const a = (L * L - r * r + d * d) / (2 * d);
  const h = Math.sqrt(Math.max(0, L * L - a * a));
  const mx = PIV.x + (a * dx) / d, my = PIV.y + (a * dy) / d;
  const p1 = { x: mx + (h * dy) / d, y: my - (h * dx) / d };
  const p2 = { x: mx - (h * dy) / d, y: my + (h * dx) / d };
  const p = p1.y > p2.y ? p1 : p2;
  return Math.atan2(p.y - PIV.y, p.x - PIV.x);
}
const A_START = armAngle(R_START), A_END = armAngle(R_END);

// `volumeLocked` lets the parent force the bit-perfect lock (P4.5 may pass it
// explicitly); when omitted we derive it from the backend's `bitPerfect` flag.
// The foobar backend leaves `bitPerfect` unset, so its volume stays fully live.
export function Turntable({ p, volumeLocked }: { p: PlayerView; volumeLocked?: boolean }) {
  const svg = useRef<SVGSVGElement>(null);
  const knob = useRef<{ y: number; v: number } | null>(null);
  const playing = p.state === "playing";
  const frac = p.duration > 0 ? Math.min(1, p.position / p.duration) : 0;
  const onRecord = p.state !== "stopped" && !!p.track;
  const ang = onRecord ? A_START + (A_END - A_START) * frac : REST;
  const deg = (ang * 180) / Math.PI;

  const toSvg = (e: { clientX: number; clientY: number }) => {
    const s = svg.current!;
    const pt = s.createSVGPoint();
    pt.x = e.clientX;
    pt.y = e.clientY;
    return pt.matrixTransform(s.getScreenCTM()!.inverse());
  };
  // needle drop: clicking a groove seeks to that point of the side
  const drop = (e: { clientX: number; clientY: number }) => {
    if (!p.duration) return;
    const q = toSvg(e);
    const r = Math.hypot(q.x - C.x, q.y - C.y);
    if (r < R_END || r > R_START + 6) return;
    p.seek(((R_START - Math.min(R_START, r)) / (R_START - R_END)) * p.duration);
  };
  const key = (fn: () => void) => ({
    role: "button",
    tabIndex: 0,
    onClick: fn,
    onKeyDown: (e: { key: string; preventDefault: () => void }) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        fn();
      }
    },
    style: { cursor: "pointer" },
  });

  // Bit-perfect lock: the knob sits at 0 dB (= max, 1.0) and setVolume is never
  // called, because gain must be unity to stay bit-perfect (adjust on the DAC/amp).
  const locked = volumeLocked ?? p.bitPerfect === true;
  const vol = locked ? 1 : p.volume;
  const setVol = (v: number) => {
    if (!locked) p.setVolume(v);
  };
  const vAng = -135 + vol * 270;
  const labelText = (p.track?.title || "—").slice(0, 22);

  return (
    <svg ref={svg} viewBox={`0 0 ${W} ${H}`} className="deck-svg" role="img" aria-label="Đầu đĩa than">
      <defs>
        <linearGradient id="alu" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#ece7dc" />
          <stop offset=".55" stopColor="#d9d3c6" />
          <stop offset="1" stopColor="#c4bdae" />
        </linearGradient>
        <pattern id="brush" width="6" height="640" patternUnits="userSpaceOnUse" patternTransform="rotate(90)">
          <rect width="6" height="640" fill="transparent" />
          <line x1="1" y1="0" x2="1" y2="640" stroke="#fff" strokeOpacity=".18" />
          <line x1="4" y1="0" x2="4" y2="640" stroke="#000" strokeOpacity=".035" />
        </pattern>
        <radialGradient id="vinyl">
          <stop offset="0" stopColor="#1b1a19" />
          <stop offset="1" stopColor="#0d0c0c" />
        </radialGradient>
        <pattern id="groove" width="400" height="400" patternUnits="userSpaceOnUse" x={C.x - 200} y={C.y - 200}>
          {Array.from({ length: 52 }, (_, i) => (
            <circle key={i} cx="200" cy="200" r={R_END + 4 + i * 2} fill="none" stroke="#fff" strokeOpacity={i % 9 === 8 ? 0 : 0.045} strokeWidth=".6" />
          ))}
        </pattern>
        <radialGradient id="platter">
          <stop offset=".9" stopColor="#9d988e" />
          <stop offset="1" stopColor="#6f6b63" />
        </radialGradient>
        <linearGradient id="armg" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#f6f4ef" />
          <stop offset="1" stopColor="#a9a59c" />
        </linearGradient>
        <radialGradient id="knobg" cx=".4" cy=".35">
          <stop offset="0" stopColor="#f3efe6" />
          <stop offset="1" stopColor="#9b958a" />
        </radialGradient>
        <clipPath id="labelClip"><circle cx={C.x} cy={C.y} r={R_LABEL} /></clipPath>
        <filter id="soft" x="-20%" y="-20%" width="140%" height="140%">
          <feDropShadow dx="0" dy="10" stdDeviation="9" floodColor="#000" floodOpacity=".45" />
        </filter>
        <filter id="lift"><feDropShadow dx="5" dy="9" stdDeviation="4" floodColor="#000" floodOpacity=".45" /></filter>
      </defs>

      {/* smoked acrylic lid, propped open behind the plinth (drag handle) */}
      <rect data-tauri-drag-region x="36" y="4" width="568" height="40" rx="6" fill="#3a3632" fillOpacity=".93" stroke="#8c857a" strokeOpacity=".5" style={{ cursor: "grab" }} />
      <rect x="48" y="10" width="544" height="6" rx="3" fill="#fff" fillOpacity=".08" pointerEvents="none" />

      {/* plinth (drag handle) */}
      <g filter="url(#soft)">
        <rect x="20" y="36" width="600" height="452" rx="22" fill="url(#alu)" />
      </g>
      <rect data-tauri-drag-region x="20" y="36" width="600" height="452" rx="22" fill="url(#brush)" style={{ cursor: "grab" }} />
      <rect x="21" y="37" width="598" height="450" rx="21" fill="none" stroke="#fff" strokeOpacity=".6" pointerEvents="none" />
      {[110, 530].map((x) => (
        <rect key={x} x={x - 18} y="30" width="36" height="16" rx="3" fill="#2d2a27" pointerEvents="none" />
      ))}
      {[[44, 60], [596, 60], [44, 464], [596, 464]].map(([x, y]) => (
        <g key={x + "-" + y} pointerEvents="none">
          <circle cx={x} cy={y} r="4.2" fill="#a59f93" />
          <line x1={x - 2.5} y1={y} x2={x + 2.5} y2={y} stroke="#6c665c" strokeWidth="1" />
        </g>
      ))}
      <g pointerEvents="none">
        <text x="58" y="84" fontFamily="Newsreader, Georgia, serif" fontStyle="italic" fontSize="22" fill="#3a3632" fillOpacity=".85">Lyra</text>
        <text x="59" y="96" fontFamily="'DM Mono', monospace" fontSize="6.5" letterSpacing="3" fill="#5c564d" fillOpacity=".8">TURNTABLE</text>
      </g>

      {/* platter with strobe dots */}
      <circle cx={C.x} cy={C.y + 3} r={R_PLATTER} fill="#000" fillOpacity=".25" />
      <circle cx={C.x} cy={C.y} r={R_PLATTER} fill="url(#platter)" />
      <g className={playing ? "spin" : "spin paused"} style={{ transformOrigin: `${C.x}px ${C.y}px` }}>
        {Array.from({ length: 90 }, (_, i) => {
          const a = (i / 90) * Math.PI * 2;
          const x = C.x + Math.cos(a) * (R_PLATTER - 6), y = C.y + Math.sin(a) * (R_PLATTER - 6);
          return <rect key={i} x={x - 1} y={y - 1.6} width="2" height="3.2" fill="#3d3a35" transform={`rotate(${(a * 180) / Math.PI} ${x} ${y})`} />;
        })}
        <circle cx={C.x} cy={C.y} r={R_VINYL} fill="url(#vinyl)" />
        <circle cx={C.x} cy={C.y} r={R_VINYL} fill="url(#groove)" />
        <circle cx={C.x} cy={C.y} r={R_VINYL - 2} fill="none" stroke="#2a2826" strokeWidth="3" />
        <circle cx={C.x} cy={C.y} r={R_LABEL} fill="#9c2f22" />
        {p.art ? (
          <image href={p.art} x={C.x - R_LABEL} y={C.y - R_LABEL} width={R_LABEL * 2} height={R_LABEL * 2} clipPath="url(#labelClip)" preserveAspectRatio="xMidYMid slice" />
        ) : (
          <g fontFamily="'DM Mono', monospace" textAnchor="middle" fill="#f3e6d2">
            <circle cx={C.x} cy={C.y} r={R_LABEL - 6} fill="none" stroke="#f3e6d2" strokeOpacity=".35" />
            <text x={C.x} y={C.y - 26} fontSize="7.5" letterSpacing="2">LYRA · SIDE A</text>
            <text x={C.x} y={C.y + 30} fontSize="8.5" fontFamily="'Newsreader', serif" fontStyle="italic">{labelText}</text>
          </g>
        )}
      </g>
      {/* sheen stays still while the record turns */}
      <path d={`M ${C.x} ${C.y} L ${C.x - 130} ${C.y - 130} A ${R_VINYL} ${R_VINYL} 0 0 1 ${C.x - 40} ${C.y - 180} Z`} fill="#fff" fillOpacity=".07" pointerEvents="none" />
      <path d={`M ${C.x} ${C.y} L ${C.x + 130} ${C.y + 130} A ${R_VINYL} ${R_VINYL} 0 0 1 ${C.x + 40} ${C.y + 180} Z`} fill="#fff" fillOpacity=".05" pointerEvents="none" />
      <circle cx={C.x} cy={C.y} r="5" fill="#d8d3c8" stroke="#5a564f" />
      {/* needle-drop target */}
      <circle cx={C.x} cy={C.y} r={R_START + 6} fill="transparent" style={{ cursor: p.duration ? "crosshair" : "default" }} onClick={drop} />

      {/* arm rest + lift lever */}
      <rect x="478" y="352" width="34" height="14" rx="5" fill="#3a3632" transform="rotate(11 495 359)" />
      <path d="M 590 160 l 0 46" stroke="#3a3632" strokeWidth="6" strokeLinecap="round" />
      <circle cx="590" cy="160" r="6" fill="#d9d3c6" stroke="#3a3632" strokeWidth="2" />

      {/* tonearm */}
      <g style={{ transition: "transform 1.1s cubic-bezier(.65,0,.35,1)", transform: `rotate(${deg}deg)`, transformOrigin: `${PIV.x}px ${PIV.y}px` }} filter="url(#lift)" pointerEvents="none">
        <rect x={PIV.x - 62} y={PIV.y - 11} width="40" height="22" rx="5" fill="#4a4540" />
        <rect x={PIV.x - 58} y={PIV.y - 11} width="3" height="22" fill="#6b655d" />
        <rect x={PIV.x - 22} y={PIV.y - 3.5} width={L - 6} height="7" rx="3.5" fill="url(#armg)" stroke="#7d786f" strokeWidth=".8" />
        <g transform={`translate(${PIV.x + L - 18} ${PIV.y}) rotate(-22)`}>
          <rect x="-6" y="-11" width="40" height="22" rx="3" fill="#2f2c29" />
          <rect x="22" y="-14" width="16" height="6" rx="2" fill="#2f2c29" />
          <rect x="10" y="-5" width="8" height="10" fill="#9c2f22" />
        </g>
      </g>
      <circle cx={PIV.x} cy={PIV.y} r="26" fill="#cfc9bc" stroke="#8f897e" />
      <circle cx={PIV.x} cy={PIV.y} r="16" fill="url(#knobg)" stroke="#7d786f" />
      <circle cx={PIV.x} cy={PIV.y} r="4" fill="#3a3632" />

      {/* speed switch (decorative) + power lamp */}
      <g fontFamily="'DM Mono', monospace" fontSize="9" fill="#5a554d" pointerEvents="none">
        <text x="340" y="434">33</text>
        <text x="368" y="434">45</text>
        <rect x="340" y="440" width="44" height="12" rx="6" fill="#bdb6a8" />
        <circle cx="347" cy="446" r="5" fill="#3a3632" />
        <circle cx="410" cy="446" r="5" fill={playing ? "#f2a33a" : "#7c766b"} className={playing ? "lamp" : ""} />
        <text x="421" y="449">POWER</text>
      </g>

      {/* bit-perfect badge, near the POWER lamp (hidden for the foobar backend) */}
      <Badge p={p} x={455} y={408} />

      {/* piano keys */}
      <rect x="56" y="420" width="188" height="52" rx="8" fill="#2c2926" />
      {[
        { x: 62, label: "PREV", icon: "M 14 0 L 4 7 L 14 14 Z M 2 0 L 2 14", fn: p.prev, aria: "Bài trước" },
        { x: 122, label: playing ? "PAUSE" : "PLAY", icon: playing ? "M 3 0 L 3 14 M 11 0 L 11 14" : "M 3 0 L 14 7 L 3 14 Z", fn: p.playPause, aria: playing ? "Tạm dừng" : "Phát" },
        { x: 182, label: "NEXT", icon: "M 2 0 L 12 7 L 2 14 Z M 14 0 L 14 14", fn: p.next, aria: "Bài tiếp" },
      ].map((k) => (
        <g key={k.x} {...key(k.fn)} aria-label={k.aria} className="pkey">
          <rect x={k.x} y="425" width="56" height="42" rx="5" fill="#efeae0" stroke="#b5ae9f" />
          <g transform={`translate(${k.x + 21} 432)`} stroke="#2c2926" strokeWidth="2.2" fill="#2c2926" strokeLinejoin="round">
            <path d={k.icon} />
          </g>
          <text x={k.x + 28} y="460" textAnchor="middle" fontFamily="'DM Mono', monospace" fontSize="7.5" fill="#6b655b" letterSpacing="1.2">{k.label}</text>
        </g>
      ))}

      {/* volume knob: drag up/down, wheel, or arrow keys. Locked at 0 dB in bit-perfect mode. */}
      <g
        role="slider"
        tabIndex={0}
        aria-label="Âm lượng"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(vol * 100)}
        aria-disabled={locked}
        style={{ cursor: locked ? "not-allowed" : "ns-resize" }}
        onPointerDown={(e) => {
          if (locked) return;
          (e.target as Element).setPointerCapture(e.pointerId);
          knob.current = { y: e.clientY, v: p.volume };
        }}
        onPointerMove={(e) => knob.current && setVol(Math.max(0, Math.min(1, knob.current.v + (knob.current.y - e.clientY) / 160)))}
        onPointerUp={() => (knob.current = null)}
        onWheel={(e) => setVol(Math.max(0, Math.min(1, p.volume - Math.sign(e.deltaY) * 0.04)))}
        onKeyDown={(e) => {
          if (e.key === "ArrowUp" || e.key === "ArrowRight") setVol(Math.min(1, p.volume + 0.05));
          if (e.key === "ArrowDown" || e.key === "ArrowLeft") setVol(Math.max(0, p.volume - 0.05));
        }}
      >
        {locked && <title>{S.volumeLockedTip}</title>}
        {Array.from({ length: 11 }, (_, i) => {
          const a = ((-135 + i * 27 - 90) * Math.PI) / 180;
          return <line key={i} x1={560 + Math.cos(a) * 34} y1={430 + Math.sin(a) * 34} x2={560 + Math.cos(a) * 39} y2={430 + Math.sin(a) * 39} stroke="#6b655b" strokeWidth={i % 5 === 0 ? 1.6 : 0.9} />;
        })}
        <circle cx="560" cy="433" r="28" fill="#000" fillOpacity=".25" />
        <circle cx="560" cy="430" r="28" fill="url(#knobg)" stroke="#7d786f" />
        <g transform={`rotate(${vAng} 560 430)`}>
          <line x1="560" y1="430" x2="560" y2="408" stroke="#9c2f22" strokeWidth="3" strokeLinecap="round" />
        </g>
        <text x="560" y="476" textAnchor="middle" fontFamily="'DM Mono', monospace" fontSize="7.5" fill="#6b655b" letterSpacing="1.2">VOLUME</text>
      </g>
    </svg>
  );
}
