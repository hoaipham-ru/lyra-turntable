// foobar2000 via Beefweb. Inside Tauri the HTTP plugin makes the requests from Rust, so CORS never applies.
import { useCallback, useEffect, useRef, useState } from "react";
import { fetch as tauriFetch } from "@tauri-apps/plugin-http";
import { BASE, isDefault, type PlayerBackend, type Playlist, type Track } from "./PlayerBackend";

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const doFetch: typeof fetch = inTauri ? (tauriFetch as unknown as typeof fetch) : fetch;

const COLS = ["%artist%", "%title%", "%album%", "%length_seconds%", "%filename%"];
// Beefweb wants ONE comma-separated columns parameter
const q = (cols: string[]) => "columns=" + cols.map(encodeURIComponent).join(",");
const toTrack = (c: string[], key: string): Track => {
  const title = c[1] || c[4] || "Unknown title";
  const album = c[2] && c[2] !== "?" && c[2] !== title ? c[2] : "";
  return { artist: c[0] && c[0] !== "?" ? c[0] : "", title, album, length: +c[3] || 0, key };
};
const dbToLin = (db: number) => Math.max(0, Math.min(1, Math.pow(10, db / 40)));
const linToDb = (v: number, min: number) => (v <= 0.001 ? min : Math.max(min, 40 * Math.log10(v)));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export function useFoobarBackend(base = BASE): PlayerBackend {
  const [connected, setConnected] = useState(false);
  const [status, setStatus] = useState("Waiting for foobar2000…");
  const [state, setState] = useState<PlayerBackend["state"]>("stopped");
  const [track, setTrack] = useState<Track | null>(null);
  const [pos, setPos] = useState(0);
  const [dur, setDur] = useState(0);
  const [vol, setVol] = useState(0.7);
  const [volRange, setVolRange] = useState({ type: "db", min: -100, max: 0 });
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [list, setList] = useState<Track[]>([]);
  const [active, setActive] = useState<{ pl: string | null; index: number }>({ pl: null, index: -1 });
  const [art, setArt] = useState<string | null>(null);
  const lastKey = useRef("");
  const selRef = useRef<string | null>(null);
  selRef.current = selected;

  const api = useCallback(
    async (path: string, init?: RequestInit) => {
      const r = await doFetch(base + "/api" + path, {
        ...init,
        headers: init?.body ? { "Content-Type": "application/json" } : undefined,
      });
      if (!r.ok) throw new Error(String(r.status));
      return r.status === 204 || r.status === 202 ? null : r.json();
    },
    [base],
  );
  const post = useCallback(
    (path: string, body?: unknown) => api(path, { method: "POST", body: body ? JSON.stringify(body) : undefined }).catch(() => null),
    [api],
  );

  const loadPlaylists = useCallback(async () => {
    const r = await api("/playlists");
    const pls = (r?.playlists || []) as Playlist[];
    setPlaylists(pls);
    const cur = pls.find((p) => p.isCurrent) || pls[0];
    if (cur) setSelected(cur.id);
    return cur?.id ?? null;
  }, [api]);

  const loadList = useCallback(
    async (id: string) => {
      const r = await api(`/playlists/${encodeURIComponent(id)}/items/0:2000?${q(COLS)}`);
      const items = (r?.playlistItems?.items || []) as { columns: string[] }[];
      setList(items.map((it, i) => toTrack(it.columns, id + ":" + i)));
    },
    [api],
  );

  const refresh = useCallback(async () => {
    const id = await loadPlaylists().catch(() => null);
    if (id) await loadList(id).catch(() => {});
  }, [loadPlaylists, loadList]);

  // album art: fetched through the same HTTP client and shown as a blob URL
  const loadArt = useCallback(async () => {
    try {
      const r = await doFetch(base + "/api/artwork/current");
      if (!r.ok) return setArt(null);
      const url = URL.createObjectURL(await r.blob());
      setArt((old) => {
        if (old) URL.revokeObjectURL(old);
        return url;
      });
    } catch {
      setArt(null);
    }
  }, [base]);

  useEffect(() => {
    let alive = true;
    let n = 0;
    const tick = async () => {
      try {
        const r = await api(`/player?${q(COLS)}`);
        if (!alive) return;
        const p = r.player;
        setConnected(true);
        setStatus(`foobar2000 ${p.info?.version || ""} · Beefweb ${p.info?.pluginVersion || ""}`);
        setState(p.playbackState);
        const a = p.activeItem || {};
        setPos(a.position || 0);
        setDur(a.duration || 0);
        setActive({ pl: a.playlistId || null, index: a.index ?? -1 });
        if (p.volume) {
          setVolRange({ type: p.volume.type, min: p.volume.min, max: p.volume.max });
          setVol(p.volume.type === "db" ? dbToLin(p.volume.value) : (p.volume.value - p.volume.min) / (p.volume.max - p.volume.min || 1));
        }
        const c: string[] = a.columns || [];
        const key = `${a.playlistId}:${a.index}:${c[1]}`;
        setTrack(a.index >= 0 && c.length ? toTrack(c, key) : null);
        if (key !== lastKey.current) {
          lastKey.current = key;
          loadArt();
        }
        if (n++ % 3 === 0) await refresh(); // playlists change from foobar2000 too
      } catch {
        if (!alive) return;
        setConnected(false);
        setStatus("Waiting for foobar2000 + Beefweb at " + base);
      }
    };
    tick();
    const id = setInterval(tick, 1000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, [api, base, loadArt, refresh]);

  return {
    connected,
    status,
    state,
    track,
    position: pos,
    duration: dur,
    volume: vol,
    art: connected && track ? art : null,
    playlists,
    selected,
    list,
    current: active.pl && active.pl === selected ? active.index : -1,
    playPause: () => void post("/player/play-pause"),
    next: () => void post("/player/next"),
    prev: () => void post("/player/previous"),
    seek: (s) => {
      setPos(s);
      void post("/player", { position: s });
    },
    setVolume: (v) => {
      setVol(v);
      const value = volRange.type === "db" ? linToDb(v, volRange.min) : volRange.min + v * (volRange.max - volRange.min);
      void post("/player", { volume: value });
    },
    playAt: (i) => {
      if (selected) void post(`/player/play/${encodeURIComponent(selected)}/${i}`);
    },
    // making a playlist "current" in foobar2000 is also what decides where /add puts new files
    selectPlaylist: async (id) => {
      setSelected(id);
      selRef.current = id; // waitForItems right after must already look at this playlist
      await post("/playlists", { current: id });
      await loadList(id).catch(() => {});
    },
    newPlaylist: async (title) => {
      const r = await api("/playlists/add", { method: "POST", body: JSON.stringify({ title, setCurrent: true }) }).catch(() => null);
      await refresh();
      return r?.id ?? selRef.current;
    },
    removeItems: async (idx) => {
      if (!selected) return;
      await post(`/playlists/${encodeURIComponent(selected)}/items/remove`, { items: idx });
      await loadList(selected).catch(() => {});
    },
    clearPlaylist: async () => {
      if (!selected) return;
      await post(`/playlists/${encodeURIComponent(selected)}/clear`);
      await loadList(selected).catch(() => {});
    },
    refresh,
    // after /add, foobar2000 fills the playlist asynchronously
    waitForItems: async (atLeast, ms = 6000) => {
      const t0 = Date.now();
      while (Date.now() - t0 < ms) {
        const id = selRef.current;
        if (id) {
          const r = await api("/playlists").catch(() => null);
          const pl = (r?.playlists || []).find((x: Playlist) => x.id === id);
          if ((pl?.itemCount ?? 0) >= atLeast) {
            await loadList(id).catch(() => {});
            return true;
          }
        }
        await sleep(250);
      }
      return false;
    },
    // find the Default playlist, or create an empty one (without making it current)
    ensureDefault: async () => {
      const find = async () => ((await api("/playlists").catch(() => null))?.playlists || []).find((x: Playlist) => isDefault(x.title)) as Playlist | undefined;
      let pl = await find();
      if (!pl) {
        await post("/playlists/add", { title: "Default Playlist", setCurrent: false });
        pl = await find();
      }
      return pl ? { id: pl.id, count: pl.itemCount } : null;
    },
    clearById: async (id) => {
      await post(`/playlists/${encodeURIComponent(id)}/clear`);
      if (id === selRef.current) await loadList(id).catch(() => {});
    },
    playIn: (id, i) => void post(`/player/play/${encodeURIComponent(id)}/${i}`),
    playingPlaylist: active.pl,
  };
}

// Backward-compat alias: callers that used `useFoobar` keep working.
export const useFoobar = useFoobarBackend;
