import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as pick } from "@tauri-apps/plugin-dialog";
import { Turntable } from "./Turntable";
import { Settings } from "./Settings";
import { S } from "./strings";
import { isDefault } from "./backend/PlayerBackend";
import { useFoobar } from "./backend/FoobarBackend";

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

type Ask = { title: string; label: string; value: string; ok: string; done: (v: string) => void } | null;

export default function App() {
  const p = useFoobar();
  const [pinned, setPinned] = useState(false);
  const [menu, setMenu] = useState(false);
  const [ask, setAsk] = useState<Ask>(null);
  const [dropping, setDropping] = useState(false);
  const [toast, setToast] = useState("");
  const [confirmClear, setConfirmClear] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const listRef = useRef<HTMLOListElement>(null);
  const pRef = useRef(p);
  pRef.current = p;

  const say = (t: string) => {
    setToast(t);
    setTimeout(() => setToast((x) => (x === t ? "" : x)), 2600);
  };

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
    const OWN_DRAG = "input,select,textarea,[role=slider],.menu,.dialog";
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
  const ready = useRef<{ promise: Promise<string | null>; resolve: (id: string | null) => void } | null>(null);
  if (!ready.current) {
    let resolve!: (id: string | null) => void;
    ready.current = { promise: new Promise((r) => (resolve = r)), resolve };
  }
  const initDone = useRef(false);
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
  }, [p.connected]);

  // "Mở" / "Open with": add to Default Playlist and play the first of the new songs
  const openPaths = useCallback(async (paths: string[]) => {
    if (!paths.length) return;
    const id = await ready.current!.promise;
    if (!id) return say("Chưa nối được foobar2000");
    await pRef.current.selectPlaylist(id);
    const before = (await pRef.current.ensureDefault())?.count ?? 0;
    await fbAdd(paths);
    if (await pRef.current.waitForItems(before + 1)) pRef.current.playIn(id, before);
    say(`Đang phát trong Default Playlist`);
  }, []);
  // "Thêm": append to the playlist on screen; adding to Default by hand means "keep it next time"
  const addPaths = useCallback(async (paths: string[]) => {
    if (!paths.length) return;
    const pl = pRef.current;
    const sel = pl.playlists.find((x) => x.id === pl.selected);
    if (sel && isDefault(sel.title)) store.set(TOUCHED, "1");
    const before = pl.list.length;
    await fbAdd(paths);
    await pRef.current.waitForItems(before + 1);
    say(`Đã thêm ${paths.length} mục vào ${sel?.title ?? "playlist"}`);
  }, []);
  const clearShown = () => {
    const sel = p.playlists.find((x) => x.id === p.selected);
    if (sel && isDefault(sel.title)) store.set(TOUCHED, "0");
    void p.clearPlaylist();
  };

  const actions = {
    open: async () => {
      const r = await pick({ multiple: true, title: "Mở file nhạc", filters: [{ name: "Âm thanh", extensions: AUDIO }] });
      if (r) await openPaths(([] as string[]).concat(r));
    },
    addFiles: async () => {
      const r = await pick({ multiple: true, title: "Thêm file vào playlist", filters: [{ name: "Âm thanh", extensions: AUDIO }] });
      if (r) await addPaths(([] as string[]).concat(r));
    },
    addFolder: async () => {
      const r = await pick({ directory: true, multiple: true, title: "Thêm thư mục vào playlist" });
      if (r) await addPaths(([] as string[]).concat(r));
    },
    addLocation: () =>
      setAsk({ title: "Thêm địa chỉ", label: "URL (radio, stream, file trên mạng)", value: "https://", ok: "Thêm", done: (v) => v.trim() && addPaths([v.trim()]) }),
    newPlaylist: () =>
      setAsk({ title: "Playlist mới", label: "Tên playlist", value: `Playlist ${p.playlists.length + 1}`, ok: "Tạo", done: (v) => p.newPlaylist(v.trim() || "Playlist mới") }),
    loadPlaylist: async () => {
      const r = await pick({ multiple: false, title: "Mở playlist", filters: [{ name: "Playlist", extensions: PLAYLIST_EXT }] });
      if (typeof r === "string") {
        await p.newPlaylist(baseName(r));
        await addPaths([r]);
      }
    },
    savePlaylist: () => fbCommand("Save playlist..."),
    preferences: () => fbCommand("Preferences"),
    quit: () => invoke("quit_app"),
  };
  const items: ({ label: string; key?: string; run: () => unknown } | null)[] = [
    { label: "Mở file…", key: "Ctrl+O", run: actions.open },
    { label: "Thêm file…", run: actions.addFiles },
    { label: "Thêm thư mục…", run: actions.addFolder },
    { label: "Thêm địa chỉ…", key: "Ctrl+U", run: actions.addLocation },
    null,
    { label: "Playlist mới", key: "Ctrl+N", run: actions.newPlaylist },
    { label: "Mở playlist…", run: actions.loadPlaylist },
    { label: "Lưu playlist…", key: "Ctrl+S", run: actions.savePlaylist },
    null,
    { label: "Cài đặt foobar2000", key: "Ctrl+P", run: actions.preferences },
    null,
    { label: "Thoát", run: actions.quit },
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
        const map: Record<string, () => unknown> = { o: actRef.current.open, u: actRef.current.addLocation, n: actRef.current.newPlaylist, s: actRef.current.savePlaylist, p: actRef.current.preferences };
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

  const frac = p.duration ? p.position / p.duration : 0;

  return (
    <div className={"shell " + (showList ? "open" : "closed")} onClick={() => setMenu(false)}>
      <section className="deck" aria-label="Đầu đĩa">
        <Turntable p={p} />
      </section>

      <button className="drawer" onClick={toggleSidebar} aria-expanded={showList} aria-controls="sidebar"
        title={showList ? "Thu thẻ bài hát vào" : "Mở thẻ bài hát"}>
        <span aria-hidden>{showList ? "‹" : "›"}</span>
      </button>

      <aside id="sidebar" className="card" aria-label="Thẻ bài hát">
        <div className="bar" data-tauri-drag-region>
          <div className="menuwrap" onClick={(e) => e.stopPropagation()}>
            <button className={"filebtn " + (menu ? "on" : "")} aria-haspopup="menu" aria-expanded={menu} onClick={() => setMenu(!menu)}>Tệp ▾</button>
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
          <span className={"status " + (p.connected ? "ok" : "")} data-tauri-drag-region title={p.status}>{p.connected ? "foobar2000" : "đang chờ…"}</span>
          <div className="winbtns">
            <button className="wb" title={S.settingsBtn} aria-label={S.settingsBtn} onClick={() => setSettingsOpen(true)}>⚙</button>
            <button className={"wb " + (pinned ? "on" : "")} title={pinned ? "Bỏ ghim" : "Luôn nổi trên cùng"} aria-pressed={pinned}
              onClick={async () => { await win?.setAlwaysOnTop(!pinned); setPinned(!pinned); }}>⊤</button>
            <button className="wb" title="Thu nhỏ" onClick={() => win?.minimize()}>–</button>
            <button className="wb close" title="Đóng (tắt luôn foobar2000 nếu app đã bật nó)" onClick={() => win?.close()}>×</button>
          </div>
        </div>

        <p className="kicker">{p.state === "playing" ? "Đang quay" : p.state === "paused" ? "Tạm dừng" : "Đứng yên"}</p>
        <h1 className="title">{p.track?.title || (p.connected ? "Chưa phát bài nào" : "Đang bật foobar2000…")}</h1>
        {p.track && (p.track.artist || p.track.album) && <p className="artist">{[p.track.artist, p.track.album].filter(Boolean).join(" — ")}</p>}

        <div className="seekrow">
          <span>{fmt(p.position)}</span>
          <input type="range" min={0} max={1000} value={Math.round(frac * 1000)}
            onChange={(e) => p.duration && p.seek((+e.target.value / 1000) * p.duration)} aria-label="Vị trí trong bài" disabled={!p.duration} />
          <span>{fmt(p.duration)}</span>
        </div>

        {(<>
        <div className="plhead">
          <label className="sr" htmlFor="pl">Playlist</label>
          <select id="pl" value={p.selected ?? ""} onChange={(e) => { setConfirmClear(false); void p.selectPlaylist(e.target.value); }}>
            {p.playlists.map((pl) => (
              <option key={pl.id} value={pl.id}>{pl.title} · {pl.itemCount}</option>
            ))}
          </select>
          <button className="mini" title="Playlist mới (Ctrl+N)" onClick={actions.newPlaylist}>＋</button>
          <button className={"mini " + (confirmClear ? "danger" : "")} title="Xóa hết bài trong playlist này" disabled={!p.list.length}
            onClick={() => { if (confirmClear) { setConfirmClear(false); clearShown(); } else { setConfirmClear(true); setTimeout(() => setConfirmClear(false), 3000); } }}>
            {confirmClear ? "Chắc chưa?" : "Xóa hết"}
          </button>
        </div>
        {p.list.length ? (
          <ol ref={listRef} className="list">
            {p.list.map((t, i) => (
              <li key={t.key} data-current={i === p.current}>
                <button className="row" onClick={() => p.playAt(i)}>
                  <span className="n">{String(i + 1).padStart(2, "0")}</span>
                  <span className="t">{t.title}{(t.artist || t.album) && <em>{[t.artist, t.album].filter(Boolean).join(" — ")}</em>}</span>
                  <span className="d">{t.length ? fmt(t.length) : ""}</span>
                </button>
                <button className="rm" title="Bỏ khỏi playlist" aria-label={`Bỏ ${t.title} khỏi playlist`} onClick={() => void p.removeItems([i])}>×</button>
              </li>
            ))}
          </ol>
        ) : (
          <div className="empty">
            <p>Playlist trống.</p>
            <p>Kéo file hoặc thư mục nhạc vào cửa sổ, hoặc dùng <b>Tệp → Thêm file…</b></p>
          </div>
        )}
        </>)}
        {toast && <div className="toast" role="status">{toast}</div>}
      </aside>

      {dropping && <div className="dropzone">Thả vào để thêm vào playlist</div>}

      {settingsOpen && <Settings onClose={() => setSettingsOpen(false)} />}

      {ask && (
        <div className="modal" onClick={() => setAsk(null)}>
          <form className="dialog" onClick={(e) => e.stopPropagation()} onSubmit={(e) => { e.preventDefault(); ask.done(ask.value); setAsk(null); }}>
            <h2>{ask.title}</h2>
            <label htmlFor="askv">{ask.label}</label>
            <input id="askv" autoFocus value={ask.value} onChange={(e) => setAsk({ ...ask, value: e.target.value })} onFocus={(e) => e.target.select()} />
            <div className="dlgbtns">
              <button type="button" className="ghost" onClick={() => setAsk(null)}>Hủy</button>
              <button type="submit" className="primary">{ask.ok}</button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
}
