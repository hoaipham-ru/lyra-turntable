import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as pick, save as pickSave } from "@tauri-apps/plugin-dialog";
import { Turntable } from "./Turntable";
import { Settings } from "./Settings";
import { Dropdown } from "./Dropdown";
import { S } from "./strings";
import { isDefault } from "./backend/PlayerBackend";
import { useFoobar } from "./backend/FoobarBackend";
import { useNativeBackend } from "./backend/NativeBackend";

const store = {
  get: (k: string, d: string) => { try { return localStorage.getItem(k) ?? d; } catch { return d; } },
  set: (k: string, v: string) => { try { localStorage.setItem(k, v); } catch { /* storage unavailable */ } },
};
// "Default Playlist" is a scratch playlist: emptied at every start, unless the user added songs to it themselves
const TOUCHED = "tt.defaultTouched";

const fmt = (s: number) => {
  if (!isFinite(s) || s < 0) s = 0;
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), x = Math.floor(s % 60);
  return (h ? h + ":" + String(m).padStart(2, "0") : String(m)) + ":" + String(x).padStart(2, "0");
};
const inTauri = "__TAURI_INTERNALS__" in window;
const win = inTauri ? getCurrentWindow() : null;
const AUDIO = ["flac", "wav", "mp3", "m4a", "aac", "alac", "aiff", "aif", "ape", "wv", "ogg", "opus", "dsf", "dff", "iso", "cue"];
const PLAYLIST_EXT = ["m3u", "m3u8", "pls", "fpl", "cue"];
const fbAdd = (paths: string[]) => invoke("fb_add", { paths });
const fbCommand = (name: string) => invoke("fb_command", { name });
const baseName = (p: string) => p.split(/[\\/]/).pop()!.replace(/\.[^.]+$/, "");

// Which player drives the UI. Persisted by Settings.tsx (`settings_set` + the
// `tt.settings` localStorage mirror); P4.5 reads it to pick the backend hook.
type Backend = "foobar" | "native";
const SETTINGS_KEY = "tt.settings"; // localStorage mirror of the Rust settings file
// Synchronous first guess for the initial render (before the async `settings_get`
// confirms it). Outside Tauri there is no Rust side, so this is the only source.
const readBackendSync = (): Backend => {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY);
    if (raw && JSON.parse(raw).backend === "native") return "native";
  } catch {
    /* storage unavailable */
  }
  return "foobar";
};

// The Default-Playlist startup pass resolves this with the Default playlist id;
// every open/add flow awaits it. Recreated on each backend switch so the new
// backend runs its own pass.
type Ready = { promise: Promise<string | null>; resolve: (id: string | null) => void };
const makeReady = (): Ready => {
  let resolve!: (id: string | null) => void;
  return { promise: new Promise<string | null>((r) => (resolve = r)), resolve };
};

type Ask = { title: string; label: string; value: string; ok: string; done: (v: string) => void } | null;

export default function App() {
  // Backend switching (P4.5). We MOUNT BOTH hooks every render (React forbids
  // conditional hooks) and pick which one drives the UI via `backend`. The
  // "never two players" rule is enforced two ways:
  //   1. Only the SELECTED backend `p` is ever sent transport/add commands. The
  //      other backend merely polls/listens (FoobarBackend polls Beefweb; Native
  //      listens to engine events) — neither makes sound unless commanded.
  //   2. On every switch we actively STOP the backend we leave (pause foobar2000 /
  //      `engine_shutdown` the native engine), so a track left playing can't keep
  //      sounding once the other backend takes over.
  const foobarP = useFoobar();
  const nativeP = useNativeBackend();
  const [backend, setBackend] = useState<Backend>(readBackendSync);
  const p = backend === "native" ? nativeP : foobarP;
  const backendRef = useRef(backend);
  backendRef.current = backend;
  const [pinned, setPinned] = useState(false);
  const [menu, setMenu] = useState(false);
  const [ask, setAsk] = useState<Ask>(null);
  const [dropping, setDropping] = useState(false);
  const [toast, setToast] = useState("");
  const [confirmClear, setConfirmClear] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const listRef = useRef<HTMLOListElement>(null);
  // Playlist card selection + drag-to-reorder (foobar2000-style: click selects, double-click plays).
  const [sel, setSel] = useState<Set<number>>(new Set());
  const anchor = useRef(-1);
  const [drag, setDrag] = useState<{ from: number; over: number } | null>(null);
  const press = useRef<{ i: number; y: number; id: number } | null>(null);
  const justDragged = useRef(false);
  const pRef = useRef(p);
  pRef.current = p;

  const say = (t: string) => {
    setToast(t);
    setTimeout(() => setToast((x) => (x === t ? "" : x)), 2600);
  };

  // Confirm the persisted backend choice from the Rust settings file (the initial
  // render used the localStorage mirror). A restart thus honours the saved choice.
  useEffect(() => {
    if (!inTauri) return;
    void invoke<{ backend?: string }>("settings_get")
      .then((s) => setBackend(s?.backend === "native" ? "native" : "foobar"))
      .catch(() => {});
  }, []);

  // On every backend switch, STOP the backend we are leaving so two players never
  // produce sound at once (non-negotiable). Leaving foobar2000: pause it if it is
  // playing. Leaving native: shut the engine down and release its device.
  const prevBackend = useRef(backend);
  useEffect(() => {
    const from = prevBackend.current;
    if (from === backend) return;
    prevBackend.current = backend;
    if (from === "foobar") {
      if (foobarP.state === "playing") foobarP.playPause();
    } else {
      void invoke("engine_shutdown").catch(() => {});
    }
  }, [backend]);

  // Sidebar: the card slides out of a drawer tab on the turntable's right edge. Closed, the window shrinks
  // to the turntable so no transparent strip covers the desktop.
  const [showList, setShowList] = useState(() => store.get("tt.showList", "1") === "1");
  const [sliding, setSliding] = useState(false);
  const WIDE = 1040, NARROW = 690, HEIGHT = 560; // must match the fixed grid in styles.css
  useEffect(() => {
    store.set("tt.showList", showList ? "1" : "0");
  }, [showList]);
  useEffect(() => {
    if (!showList) void win?.setSize(new LogicalSize(NARROW, HEIGHT));
  }, []);
  const toggleSidebar = async () => {
    if (sliding) return;
    setSliding(true);
    if (showList) {
      setShowList(false);                       // slide the card out first…
      await new Promise((r) => setTimeout(r, 220));
      await win?.setSize(new LogicalSize(NARROW, HEIGHT)); // …then shrink the window
    } else {
      await win?.setSize(new LogicalSize(WIDE, HEIGHT));   // grow the window first…
      setShowList(true);                        // …then slide the card in
    }
    setSliding(false);
  };

  // Hold the left button and drag (past a few pixels) anywhere to move the whole window.
  // A short press stays a normal click. Controls that drag on their own are left alone.
  useEffect(() => {
    if (!win) return;
    const OWN_DRAG = "input,select,textarea,[role=slider],.menu,.dialog,.dd,.list";
    let start: { x: number; y: number } | null = null;
    const down = (e: PointerEvent) => {
      if (e.button !== 0 || (e.target as Element).closest(OWN_DRAG)) return;
      start = { x: e.clientX, y: e.clientY };
    };
    const move = (e: PointerEvent) => {
      if (!start || !(e.buttons & 1)) return;
      if (Math.hypot(e.clientX - start.x, e.clientY - start.y) < 5) return;
      start = null;
      void win.startDragging(); // Windows' own move loop: follows the cursor at any display scale
    };
    const up = () => (start = null);
    const noMenu = (e: MouseEvent) => e.preventDefault();
    addEventListener("pointerdown", down);
    addEventListener("pointermove", move);
    addEventListener("pointerup", up);
    addEventListener("contextmenu", noMenu);
    return () => {
      removeEventListener("pointerdown", down);
      removeEventListener("pointermove", move);
      removeEventListener("pointerup", up);
      removeEventListener("contextmenu", noMenu);
    };
  }, []);

  // Startup: make sure "Default Playlist" exists and is empty (unless the user filled it, or it is playing).
  // Everything that opens files waits for this, so files opened via "Open with" land in a clean Default.
  const ready = useRef<Ready | null>(null);
  if (!ready.current) ready.current = makeReady();
  const initDone = useRef(false);
  // A backend switch starts a fresh Default-Playlist startup pass for the new one
  // (reset BEFORE the init effect below, which runs right after in declared order).
  useEffect(() => {
    initDone.current = false;
    ready.current = makeReady();
  }, [backend]);
  useEffect(() => {
    if (!p.connected || initDone.current) return;
    initDone.current = true;
    (async () => {
      const d = await pRef.current.ensureDefault();
      if (d) {
        const touched = store.get(TOUCHED, "0") === "1";
        const playingHere = pRef.current.playingPlaylist === d.id && pRef.current.state !== "stopped";
        if (!touched && d.count > 0 && !playingHere) await pRef.current.clearById(d.id);
        await pRef.current.refresh();
      }
      ready.current!.resolve(d?.id ?? null);
    })();
  }, [backend, p.connected]);

  // Add `paths` to playlist `id` on the ACTIVE backend. foobar2000 adds via its
  // /add switch to the current (= selected) playlist; the native store takes an
  // explicit id. Both keep identical Default-Playlist semantics for callers.
  const addToBackend = useCallback(async (id: string, paths: string[]) => {
    if (backendRef.current === "native") {
      if (!id) return;
      await invoke("playlists_add", { id, paths });
    } else {
      await fbAdd(paths);
    }
  }, []);
  // "Mở" / "Open with": add to Default Playlist and play the first of the new songs
  const openPaths = useCallback(async (paths: string[]) => {
    if (!paths.length) return;
    const id = await ready.current!.promise;
    if (!id) return say(S.backendNotReady);
    await pRef.current.selectPlaylist(id);
    const before = (await pRef.current.ensureDefault())?.count ?? 0;
    await addToBackend(id, paths);
    if (await pRef.current.waitForItems(before + 1)) pRef.current.playIn(id, before);
    say(`Playing in Default Playlist`);
  }, [addToBackend]);
  // "Thêm": append to the playlist on screen; adding to Default by hand means "keep it next time"
  const addPaths = useCallback(async (paths: string[]) => {
    if (!paths.length) return;
    const pl = pRef.current;
    const sel = pl.playlists.find((x) => x.id === pl.selected);
    if (sel && isDefault(sel.title)) store.set(TOUCHED, "1");
    const before = pl.list.length;
    await addToBackend(pl.selected ?? "", paths);
    await pRef.current.waitForItems(before + 1);
    say(`Added ${paths.length} item(s) to ${sel?.title ?? "playlist"}`);
  }, [addToBackend]);
  const clearShown = () => {
    const sel = p.playlists.find((x) => x.id === p.selected);
    if (sel && isDefault(sel.title)) store.set(TOUCHED, "0");
    void p.clearPlaylist();
  };

  const actions = {
    open: async () => {
      const r = await pick({ multiple: true, title: "Open music files", filters: [{ name: "Audio", extensions: AUDIO }] });
      if (r) await openPaths(([] as string[]).concat(r));
    },
    addFiles: async () => {
      const r = await pick({ multiple: true, title: "Add files to playlist", filters: [{ name: "Audio", extensions: AUDIO }] });
      if (r) await addPaths(([] as string[]).concat(r));
    },
    addFolder: async () => {
      const r = await pick({ directory: true, multiple: true, title: "Add folder to playlist" });
      if (r) await addPaths(([] as string[]).concat(r));
    },
    addLocation: () =>
      setAsk({ title: "Add location", label: "URL (radio, stream, network file)", value: "https://", ok: "Add", done: (v) => v.trim() && addPaths([v.trim()]) }),
    newPlaylist: () =>
      setAsk({ title: "New playlist", label: "Playlist name", value: `Playlist ${p.playlists.length + 1}`, ok: "Create", done: (v) => p.newPlaylist(v.trim() || "New playlist") }),
    loadPlaylist: async () => {
      const r = await pick({ multiple: false, title: "Open playlist", filters: [{ name: "Playlist", extensions: PLAYLIST_EXT }] });
      if (typeof r === "string") {
        await p.newPlaylist(baseName(r));
        await addPaths([r]);
      }
    },
    savePlaylist: async () => {
      // foobar2000 opens its own Save dialog; native exports the shown playlist to m3u8.
      if (backendRef.current !== "native") return void fbCommand("Save playlist...");
      const id = pRef.current.selected;
      if (!id) return;
      const dest = await pickSave({ title: S.savePlaylistTitle, filters: [{ name: "Playlist", extensions: ["m3u8"] }] });
      if (typeof dest === "string") {
        await invoke("playlists_export_m3u", { id, path: dest }).catch(() => {});
        say(S.playlistSaved);
      }
    },
    preferences: () => fbCommand("Preferences"), // foobar2000-only; menu item hidden in native mode
    quit: () => invoke("quit_app"),
  };
  const items: ({ label: string; key?: string; run: () => unknown } | null)[] = [
    { label: "Open file…", key: "Ctrl+O", run: actions.open },
    { label: "Add files…", run: actions.addFiles },
    { label: "Add folder…", run: actions.addFolder },
    { label: "Add location…", key: "Ctrl+U", run: actions.addLocation },
    null,
    { label: "New playlist", key: "Ctrl+N", run: actions.newPlaylist },
    { label: "Open playlist…", run: actions.loadPlaylist },
    { label: "Save playlist…", key: "Ctrl+S", run: actions.savePlaylist },
    // "Cài đặt foobar2000" (foobar2000 Preferences) only applies to the foobar backend.
    ...(backend === "foobar"
      ? ([null, { label: "foobar2000 preferences", key: "Ctrl+P", run: actions.preferences }] as const)
      : []),
    null,
    { label: "Exit", run: actions.quit },
  ];
  const actRef = useRef(actions);
  actRef.current = actions;

  // files from "Open with" (first launch: pending list; later launches: event). Several files arrive as
  // several launches, so collect them for a moment and open them together.
  useEffect(() => {
    if (!inTauri) return;
    let buf: string[] = [];
    let t: number | undefined;
    const queue = (files: string[]) => {
      buf = buf.concat(files);
      clearTimeout(t);
      t = window.setTimeout(() => {
        const f = buf;
        buf = [];
        void openPaths(f);
      }, 700);
    };
    invoke<string[]>("take_pending").then((f) => f.length && queue(f));
    const un = listen<string[]>("open-files", (e) => queue(e.payload));
    return () => {
      void un.then((f) => f());
    };
  }, [openPaths]);

  // drag files or folders onto the window to add them
  useEffect(() => {
    if (!inTauri) return;
    const un = getCurrentWebview().onDragDropEvent((e) => {
      if (e.payload.type === "over" || e.payload.type === "enter") setDropping(true);
      else if (e.payload.type === "leave") setDropping(false);
      else if (e.payload.type === "drop") {
        setDropping(false);
        void addPaths(e.payload.paths);
      }
    });
    return () => {
      void un.then((f) => f());
    };
  }, [addPaths]);

  useEffect(() => {
    const k = (e: KeyboardEvent) => {
      if (ask || settingsOpen) return;
      if (e.ctrlKey && !e.shiftKey && !e.altKey) {
        const map: Record<string, () => unknown> = { o: actRef.current.open, u: actRef.current.addLocation, n: actRef.current.newPlaylist, s: actRef.current.savePlaylist };
        if (backendRef.current === "foobar") map.p = actRef.current.preferences; // Ctrl+P: foobar2000 only
        const f = map[e.key.toLowerCase()];
        if (f) { e.preventDefault(); setMenu(false); void f(); }
        return;
      }
      if (e.key === "Escape") setMenu(false);
      if ((e.target as HTMLElement).closest("input,select,[role=slider],[role=button],button")) return;
      if (e.key === " ") { e.preventDefault(); p.playPause(); }
      if (e.key === "ArrowRight") p.next();
      if (e.key === "ArrowLeft") p.prev();
    };
    addEventListener("keydown", k);
    return () => removeEventListener("keydown", k);
  }, [p, ask, settingsOpen]);
  useEffect(() => {
    listRef.current?.querySelector<HTMLElement>("[data-current=true]")?.scrollIntoView({ block: "nearest" });
  }, [p.current]);
  // A different playlist or a changed list invalidates the selection.
  useEffect(() => { setSel(new Set()); anchor.current = -1; }, [p.selected, p.list.length]);

  const frac = p.duration ? p.position / p.duration : 0;

  return (
    <div className={"shell " + (showList ? "open" : "closed")} onClick={() => setMenu(false)}>
      <section className="deck" aria-label="Turntable">
        <Turntable p={p} />
      </section>

      <button className="drawer" onClick={toggleSidebar} aria-expanded={showList} aria-controls="sidebar"
        title={showList ? "Hide playlist" : "Show playlist"}>
        <span aria-hidden>{showList ? "‹" : "›"}</span>
      </button>

      <aside id="sidebar" className="card" aria-label="Playlist">
        <div className="bar" data-tauri-drag-region>
          <div className="menuwrap" onClick={(e) => e.stopPropagation()}>
            <button className={"filebtn " + (menu ? "on" : "")} aria-haspopup="menu" aria-expanded={menu} onClick={() => setMenu(!menu)}>File ▾</button>
            {menu && (
              <ul className="menu" role="menu">
                {items.map((it, i) =>
                  it ? (
                    <li key={i} role="none">
                      <button role="menuitem" onClick={() => { setMenu(false); void it.run(); }}>
                        <span>{it.label}</span>
                        {it.key && <kbd>{it.key}</kbd>}
                      </button>
                    </li>
                  ) : (
                    <li key={i} className="sep" role="separator" />
                  ),
                )}
              </ul>
            )}
          </div>
          <span className={"status " + (p.connected ? "ok" : "")} data-tauri-drag-region title={p.status}>{p.connected ? (backend === "native" ? "Lyra" : "foobar2000") : "waiting…"}</span>
          <div className="winbtns">
            <button className="wb" title={S.settingsBtn} aria-label={S.settingsBtn} onClick={() => setSettingsOpen(true)}>⚙</button>
            <button className={"wb " + (pinned ? "on" : "")} title={pinned ? "Unpin" : "Always on top"} aria-pressed={pinned}
              onClick={async () => { await win?.setAlwaysOnTop(!pinned); setPinned(!pinned); }}>⊤</button>
            <button className="wb" title="Minimize" onClick={() => win?.minimize()}>–</button>
            <button className="wb close" title="Close (also closes foobar2000 if the app started it)" onClick={() => win?.close()}>×</button>
          </div>
        </div>

        <p className="kicker">{p.state === "playing" ? "Now playing" : p.state === "paused" ? "Paused" : "Stopped"}</p>
        <h1 className="title">{p.track?.title || (p.connected ? "Nothing playing" : backend === "native" ? S.engineStarting : "Starting foobar2000…")}</h1>
        {p.track && (p.track.artist || p.track.album) && <p className="artist">{[p.track.artist, p.track.album].filter(Boolean).join(" — ")}</p>}

        <div className="seekrow">
          <span>{fmt(p.position)}</span>
          <input type="range" min={0} max={1000} value={Math.round(frac * 1000)}
            onChange={(e) => p.duration && p.seek((+e.target.value / 1000) * p.duration)} aria-label="Position in track" disabled={!p.duration} />
          <span>{fmt(p.duration)}</span>
        </div>

        {(<>
        <div className="plhead">
          <label className="sr" htmlFor="pl">Playlist</label>
          <Dropdown
            id="pl"
            className="pl"
            value={p.selected ?? ""}
            onChange={(v) => { setConfirmClear(false); void p.selectPlaylist(v); }}
            options={p.playlists.map((pl) => ({ value: pl.id, label: `${pl.title} · ${pl.itemCount}` }))}
          />
          <button className="mini" title="New playlist (Ctrl+N)" onClick={actions.newPlaylist}>＋</button>
          <button className={"mini " + (confirmClear ? "danger" : "")} title="Remove all tracks from this playlist" disabled={!p.list.length}
            onClick={() => { if (confirmClear) { setConfirmClear(false); clearShown(); } else { setConfirmClear(true); setTimeout(() => setConfirmClear(false), 3000); } }}>
            {confirmClear ? "Sure?" : "Clear"}
          </button>
        </div>
        {p.list.length ? (
          <ol ref={listRef} className={"list" + (drag ? " dragging" : "")}
            onKeyDown={(e) => {
              if (e.key === "Delete" && sel.size) { e.preventDefault(); const idx = [...sel].sort((a, b) => a - b); setSel(new Set()); void p.removeItems(idx); }
              if ((e.key === "a" || e.key === "A") && e.ctrlKey) { e.preventDefault(); setSel(new Set(p.list.map((_, i) => i))); }
            }}
            onPointerMove={(e) => {
              const pr = press.current;
              if (!pr || !p.moveItem) return;
              if (!drag && Math.abs(e.clientY - pr.y) < 6) return;
              const lis = [...(listRef.current?.children ?? [])] as HTMLElement[];
              let over = lis.length - 1;
              for (let k = 0; k < lis.length; k++) { const r = lis[k].getBoundingClientRect(); if (e.clientY < r.top + r.height / 2) { over = k; break; } }
              if (over > pr.i) over = Math.max(pr.i, over - 1); // index the item will have after the move
              if (!drag) { listRef.current?.setPointerCapture(pr.id); }
              setDrag({ from: pr.i, over });
            }}
            onPointerUp={() => {
              const d = drag;
              press.current = null;
              setDrag(null);
              if (d && p.moveItem) {
                justDragged.current = true;
                setTimeout(() => (justDragged.current = false), 0);
                if (d.from !== d.over) { setSel(new Set([d.over])); anchor.current = d.over; void p.moveItem(d.from, d.over); }
              }
            }}
            onPointerCancel={() => { press.current = null; setDrag(null); }}>
            {p.list.map((t, i) => (
              <li key={t.key} data-current={i === p.current} data-sel={sel.has(i)}
                data-drag={drag?.from === i} data-over={drag && drag.over === i && drag.from !== i ? (drag.over > drag.from ? "below" : "above") : undefined}
                onPointerDown={(e) => { if (e.button === 0 && !(e.target as Element).closest(".rm")) press.current = { i, y: e.clientY, id: e.pointerId }; }}>
                <button className="row"
                  onClick={(e) => {
                    if (justDragged.current) return;
                    const next = new Set(e.ctrlKey ? sel : []);
                    if (e.shiftKey && anchor.current >= 0) {
                      const [a, b] = [Math.min(anchor.current, i), Math.max(anchor.current, i)];
                      for (let k = a; k <= b; k++) next.add(k);
                    } else if (e.ctrlKey && sel.has(i)) next.delete(i);
                    else { next.add(i); anchor.current = i; }
                    setSel(next);
                  }}
                  onDoubleClick={() => p.playAt(i)}
                  onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); p.playAt(i); } }}
                  title="Double-click to play · drag to reorder">
                  <span className="n">{String(i + 1).padStart(2, "0")}</span>
                  <span className="t">{t.title}{(t.artist || t.album) && <em>{[t.artist, t.album].filter(Boolean).join(" — ")}</em>}</span>
                  <span className="d">{t.length ? fmt(t.length) : ""}</span>
                </button>
                <button className="rm" title="Remove from playlist" aria-label={`Remove ${t.title} from playlist`} onClick={() => void p.removeItems([i])}>×</button>
              </li>
            ))}
          </ol>
        ) : (
          <div className="empty">
            <p>Playlist is empty.</p>
            <p>Drop music files or folders onto the window, or use <b>File → Add files…</b></p>
          </div>
        )}
        </>)}
        {toast && <div className="toast" role="status">{toast}</div>}
      </aside>

      {dropping && <div className="dropzone">Drop to add to playlist</div>}

      {settingsOpen && <Settings onClose={() => setSettingsOpen(false)} onBackendChange={setBackend} />}

      {ask && (
        <div className="modal" onClick={() => setAsk(null)}>
          <form className="dialog" onClick={(e) => e.stopPropagation()} onSubmit={(e) => { e.preventDefault(); ask.done(ask.value); setAsk(null); }}>
            <h2>{ask.title}</h2>
            <label htmlFor="askv">{ask.label}</label>
            <input id="askv" autoFocus value={ask.value} onChange={(e) => setAsk({ ...ask, value: e.target.value })} onFocus={(e) => e.target.select()} />
            <div className="dlgbtns">
              <button type="button" className="ghost" onClick={() => setAsk(null)}>Cancel</button>
              <button type="submit" className="primary">{ask.ok}</button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
}
