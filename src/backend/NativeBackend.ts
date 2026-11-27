// Native lyra-engine backend (P4.3). Mirrors FoobarBackend's hook shape so App.tsx
// can swap `useFoobar()` for `useNative()` with no other change (the actual switch
// is P4.5). Instead of foobar2000 + Beefweb, this drives the native engine through
// the Tauri commands P4.1 added (engine_send / playlists_* / tags_read /
// artwork_read) and the `engine://event` event stream.
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isDefault, type PlayerBackend, type Playlist, type Track } from "./PlayerBackend";

// Outside Tauri (Playwright/jsdom) there is no engine: degrade to connected=false.
const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

// Vietnamese UI strings. Kept inline to match FoobarBackend's current style; a
// shared string table for the English/Vietnamese toggle is a polish-backlog item.
const STR = {
  waiting: "Starting Lyra Engine…",
  ready: "Lyra Engine · bit-perfect",
  unknownTitle: "Unknown title",
} as const;

// --- Engine event payloads (mirror EnginePayload in src-tauri/src/engine.rs) ---

type EngineState = "playing" | "paused" | "stopped";

type EngineEvent =
  | { kind: "state"; state: EngineState }
  | { kind: "position"; frame: number; seconds: number }
  | {
      kind: "track";
      path: string;
      title?: string | null;
      artist?: string | null;
      album?: string | null;
      durationFrames?: number | null;
      durationSeconds?: number | null;
      source?: string | null;
    }
  | { kind: "format"; sourceDesc: string; deviceDesc: string; bitPerfect: boolean; badge: string }
  | { kind: "underrun"; count: number }
  | { kind: "error"; message: string };

// DTOs returned by the Rust playlist/tag commands (src-tauri/src/library.rs).
type PlaylistDto = { id: string; title: string; isCurrent: boolean; itemCount: number };
type TrackDto = { title: string; artist: string; album: string; length: number; key: string };

// A transport command for engine_send (serde-tagged UiCommand by `type`).
type UiCommand =
  | { type: "play" | "pause" | "stop" | "next" | "prev" }
  | { type: "playAt"; index: number }
  | { type: "seek"; seconds: number }
  | { type: "setQueue"; paths: string[] };

// --- Pure mapping helpers (exported so they can be unit-tested later) ----------

// Final path component, used when a track event carries no title.
export function baseName(path: string): string {
  const m = path.split(/[\\/]/).pop();
  return m && m.length ? m : path;
}

// Build a UI Track from a `track` engine event. React needs a unique list key;
// the playlist list builds its own index-based keys, so here the key is the path.
export function trackFromEvent(ev: Extract<EngineEvent, { kind: "track" }>): Track {
  return {
    title: ev.title || baseName(ev.path) || STR.unknownTitle,
    artist: ev.artist || "",
    album: ev.album && ev.album !== ev.title ? ev.album : "",
    length: ev.durationSeconds ?? 0,
    key: ev.path,
  };
}

// Status line shown next to the transport: the format badge once negotiated.
export function statusFor(badge: string | null): string {
  return badge ? `Lyra Engine · ${badge}` : STR.ready;
}

// --- Hook ---------------------------------------------------------------------

export function useNativeBackend(): PlayerBackend {
  const [connected, setConnected] = useState(false);
  const [status, setStatus] = useState<string>(STR.waiting);
  const [state, setState] = useState<EngineState>("stopped");
  const [track, setTrack] = useState<Track | null>(null);
  const [pos, setPos] = useState(0);
  const [dur, setDur] = useState(0);
  const [art, setArt] = useState<string | null>(null);
  const [format, setFormat] = useState<string | null>(null);
  const [bitPerfect, setBitPerfect] = useState<boolean>(true);
  const [underruns, setUnderruns] = useState(0);
  // Device (endpoint) volume: applied by Windows after the render client, so the
  // stream itself stays bit-perfect. Device = the one chosen in Settings.
  const [volume, setVolumeState] = useState(1);
  const volDevice = () => { try { return (JSON.parse(localStorage.getItem("tt.settings") || "{}").deviceId as string) || null; } catch { return null; } };
  useEffect(() => {
    if (!inTauri) return;
    void invoke<number>("endpoint_volume_get", { deviceId: volDevice() }).then(setVolumeState).catch(() => {});
  }, [connected]);
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [list, setList] = useState<Track[]>([]);
  // The playlist whose paths are currently loaded into the engine queue, and the
  // index within it the engine is playing (derived from the track path).
  const [playingPl, setPlayingPl] = useState<string | null>(null);
  const [playingIdx, setPlayingIdx] = useState(-1);

  // Refs the event handler and the synchronous transport methods read without
  // re-subscribing on every state change.
  const selRef = useRef<string | null>(null);
  selRef.current = selected;
  const queuePaths = useRef<string[]>([]); // paths loaded into the engine queue
  const queueOwner = useRef<string | null>(null); // playlist id those paths came from
  const stateRef = useRef<EngineState>("stopped");
  stateRef.current = state;
  const trackPath = useRef<string>(""); // current track path, for artwork fetches

  const send = useCallback((cmd: UiCommand) => {
    if (!inTauri) return;
    void invoke("engine_send", { cmd }).catch(() => {});
  }, []);

  // Album art for the current track, as a base64 data URL (no revoke needed).
  const loadArt = useCallback(async (path: string) => {
    if (!inTauri || !path) {
      setArt(null);
      return;
    }
    try {
      const url = (await invoke("artwork_read", { path })) as string | null;
      setArt(url ?? null);
    } catch {
      setArt(null);
    }
  }, []);

  const loadList = useCallback(async (id: string) => {
    if (!inTauri) return;
    try {
      const items = (await invoke("playlists_items", { id })) as TrackDto[];
      // Index-composite keys keep React list keys unique even with duplicate paths.
      setList(items.map((it, i) => ({ ...it, key: `${id}:${i}` })));
      // Keep the picker's "title · N" count in step with the list just loaded.
      setPlaylists((pls) => pls.map((p) => (p.id === id ? { ...p, itemCount: items.length } : p)));
      if (id === queueOwner.current) queuePaths.current = items.map((it) => it.key);
    } catch {
      setList([]);
    }
  }, []);

  const loadPlaylists = useCallback(async () => {
    if (!inTauri) return null;
    try {
      const pls = (await invoke("playlists_list", {})) as PlaylistDto[];
      const sel = selRef.current;
      setPlaylists(pls.map((p) => ({ id: p.id, title: p.title, isCurrent: p.id === sel, itemCount: p.itemCount })));
      if (!sel && pls.length) {
        setSelected(pls[0].id);
        selRef.current = pls[0].id;
        return pls[0].id;
      }
      return sel;
    } catch {
      return null;
    }
  }, []);

  const refresh = useCallback(async () => {
    const id = await loadPlaylists();
    if (id) await loadList(id);
  }, [loadPlaylists, loadList]);

  // Fetch a playlist's paths from the store (used to build the engine queue).
  const pathsOf = useCallback(async (id: string): Promise<string[]> => {
    if (!inTauri) return [];
    try {
      const items = (await invoke("playlists_items", { id })) as TrackDto[];
      return items.map((it) => it.key);
    } catch {
      return [];
    }
  }, []);

  // Load `id`'s paths into the engine queue (once per owner change) and play `i`.
  const play = useCallback(
    async (id: string, i: number) => {
      const paths = await pathsOf(id);
      if (!paths.length) return;
      if (queueOwner.current !== id) {
        queueOwner.current = id;
        queuePaths.current = paths;
        send({ type: "setQueue", paths });
      }
      setPlayingPl(id);
      setPlayingIdx(i);
      send({ type: "playAt", index: i });
    },
    [pathsOf, send],
  );

  // Subscribe to the engine event stream and map events onto React state.
  useEffect(() => {
    if (!inTauri) {
      setConnected(false);
      return;
    }
    let alive = true;
    let unlisten: UnlistenFn | null = null;
    const onEvent = (ev: EngineEvent) => {
      setConnected(true);
      switch (ev.kind) {
        case "state":
          setState(ev.state);
          break;
        case "position":
          setPos(ev.seconds);
          break;
        case "track": {
          setTrack(trackFromEvent(ev));
          if (ev.durationSeconds != null) setDur(ev.durationSeconds);
          if (ev.path !== trackPath.current) {
            trackPath.current = ev.path;
            void loadArt(ev.path);
          }
          // Derive the playing index from the queue the engine is playing.
          const idx = queuePaths.current.indexOf(ev.path);
          if (idx >= 0) setPlayingIdx(idx);
          break;
        }
        case "format":
          setFormat(ev.badge);
          setBitPerfect(ev.bitPerfect);
          setStatus(statusFor(ev.badge));
          break;
        case "underrun":
          setUnderruns(ev.count);
          break;
        case "error":
          setStatus(ev.message);
          break;
      }
    };
    listen<EngineEvent>("engine://event", (e) => {
      if (alive) onEvent(e.payload);
    }).then((u) => {
      if (alive) unlisten = u;
      else u();
    });

    // Probe the engine/store so `connected` and the playlist pane fill in even
    // before the first event arrives.
    void invoke("engine_running", {})
      .then(() => setConnected(true))
      .catch(() => {});
    void refresh();

    return () => {
      alive = false;
      if (unlisten) unlisten();
    };
  }, [loadArt, refresh]);

  return {
    connected,
    status,
    state,
    track,
    position: pos,
    duration: dur,
    // Endpoint (device) volume - see `endpoint_volume_*`; never touches the samples.
    volume,
    art: connected && track ? art : null,
    playlists,
    selected,
    list,
    current: playingPl && playingPl === selected ? playingIdx : -1,
    playPause: () => send({ type: stateRef.current === "playing" ? "pause" : "play" }),
    next: () => send({ type: "next" }),
    prev: () => send({ type: "prev" }),
    seek: (s) => {
      setPos(s); // optimistic; the next position event confirms it
      send({ type: "seek", seconds: s });
    },
    setVolume: (v) => {
      setVolumeState(v);
      if (inTauri) void invoke("endpoint_volume_set", { deviceId: volDevice(), level: v }).catch(() => {});
    },
    playAt: (i) => {
      if (selected) void play(selected, i);
    },
    selectPlaylist: async (id) => {
      setSelected(id);
      selRef.current = id; // waitForItems right after must already see this playlist
      setPlaylists((pls) => pls.map((p) => ({ ...p, isCurrent: p.id === id })));
      await loadList(id);
    },
    newPlaylist: async (title) => {
      if (!inTauri) return null;
      try {
        const id = (await invoke("playlists_create", { title })) as string;
        setSelected(id);
        selRef.current = id;
        await loadPlaylists();
        await loadList(id);
        return id;
      } catch {
        return selRef.current;
      }
    },
    removeItems: async (idx) => {
      const id = selRef.current;
      if (!id) return;
      await invoke("playlists_remove", { id, indices: idx }).catch(() => {});
      await loadList(id);
    },
    moveItem: async (from, to) => {
      const id = selRef.current;
      if (!id || from === to) return;
      await invoke("playlists_reorder", { id, from, to }).catch(() => {});
      await loadList(id);
    },
    clearPlaylist: async () => {
      const id = selRef.current;
      if (!id) return;
      await invoke("playlists_clear", { id }).catch(() => {});
      await loadList(id);
    },
    refresh,
    // Native adds write synchronously, but poll like FoobarBackend so callers that
    // add then wait (Open-with / Default Playlist flow) keep working unchanged.
    waitForItems: async (atLeast, ms = 6000) => {
      if (!inTauri) return false;
      const t0 = Date.now();
      while (Date.now() - t0 < ms) {
        const id = selRef.current;
        if (id) {
          try {
            const items = (await invoke("playlists_items", { id })) as TrackDto[];
            if (items.length >= atLeast) {
              setList(items.map((it, i) => ({ ...it, key: `${id}:${i}` })));
              if (id === queueOwner.current) queuePaths.current = items.map((it) => it.key);
              return true;
            }
          } catch {
            /* keep polling */
          }
        }
        await new Promise((r) => setTimeout(r, 150));
      }
      return false;
    },
    // Find the Default Playlist, or create an empty one (without selecting it),
    // mirroring FoobarBackend so the Default Playlist startup rules are identical.
    ensureDefault: async () => {
      if (!inTauri) return null;
      try {
        let pls = (await invoke("playlists_list", {})) as PlaylistDto[];
        let def = pls.find((p) => isDefault(p.title));
        if (!def) {
          await invoke("playlists_create", { title: "Default Playlist" });
          pls = (await invoke("playlists_list", {})) as PlaylistDto[];
          def = pls.find((p) => isDefault(p.title));
        }
        return def ? { id: def.id, count: def.itemCount } : null;
      } catch {
        return null;
      }
    },
    clearById: async (id) => {
      await invoke("playlists_clear", { id }).catch(() => {});
      if (id === selRef.current) await loadList(id);
    },
    playIn: (id, i) => void play(id, i),
    playingPlaylist: playingPl,
    // Forward-looking fields the native engine fills in (P4+).
    format,
    bitPerfect,
    underruns,
  };
}

// Alias so callers can `import { useNative }` symmetrically with `useFoobar`.
export const useNative = useNativeBackend;
