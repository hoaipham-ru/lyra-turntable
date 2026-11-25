// The interface the UI depends on. Two implementations live behind it (ADR-001):
// FoobarBackend (foobar2000 + Beefweb, today's path) and, from P4, NativeBackend.
// The UI (App.tsx, Turntable.tsx) must only ever speak this surface.

export type Track = { title: string; artist: string; album: string; length: number; key: string };
export type Playlist = { id: string; title: string; isCurrent: boolean; itemCount: number };

export type PlayerBackend = {
  connected: boolean;
  status: string;
  state: "playing" | "paused" | "stopped";
  track: Track | null;
  position: number;
  duration: number;
  volume: number; // 0..1
  art: string | null;
  playlists: Playlist[];
  selected: string | null;   // playlist shown (= foobar2000's active playlist, where /add puts files)
  list: Track[];
  current: number;           // index of the playing track in the shown playlist, -1 if it plays elsewhere
  playPause: () => void;
  next: () => void;
  prev: () => void;
  seek: (sec: number) => void;
  setVolume: (v: number) => void;
  playAt: (i: number) => void;
  selectPlaylist: (id: string) => Promise<void>;
  newPlaylist: (title: string) => Promise<string | null>;
  removeItems: (idx: number[]) => Promise<void>;
  clearPlaylist: () => Promise<void>;
  refresh: () => Promise<void>;
  waitForItems: (atLeast: number, ms?: number) => Promise<boolean>;
  ensureDefault: () => Promise<{ id: string; count: number } | null>;
  clearById: (id: string) => Promise<void>;
  playIn: (id: string, i: number) => void;
  playingPlaylist: string | null;
  // Forward-looking fields for the native engine (P4+). FoobarBackend leaves them unset.
  format?: string | null;    // e.g. "FLAC 24/96 → 24/96"
  bitPerfect?: boolean;
  underruns?: number;
};

// Backward-compat alias so older `import type { PlayerView }` keeps working.
export type PlayerView = PlayerBackend;

// foobar2000 names its scratch playlist "Default" (older) or "Default Playlist"
export const isDefault = (title: string) => /^default( playlist)?$/i.test(title.trim());

export const BASE = "http://localhost:8880";
