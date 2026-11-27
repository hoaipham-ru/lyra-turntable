// Lyra Turntable: a frameless turntable window for foobar2000 (talks to it through Beefweb + foobar2000's command line).
//
// foobar2000 lifecycle:
// - app starts and foobar2000 is NOT running  -> start it, hide its window completely, remember that we did
// - app starts and foobar2000 IS running      -> just use it, never hide or close it
// - app exits                                 -> close foobar2000 only if this app started it
// foobar2000's own settings are never changed, so starting it by itself behaves as usual.
//
// Files: "Open with > Turntable" passes paths as arguments. A second launch forwards them to the running window
// (single instance). Adding files uses foobar2000's /add switch, so no Beefweb path restrictions apply.
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, RunEvent};

// Native playback engine surface (P4): engine control + events, device
// enumeration, app settings, and the playlist/tag/artwork command stubs.
mod engine;
mod library;
mod settings;
mod volume;
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow, ShowWindow,
    SW_HIDE, SW_RESTORE,
};

const FOOBAR: &str = r"C:\Program Files\foobar2000\foobar2000.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const AUDIO_EXT: &[&str] = &[
    "flac", "wav", "mp3", "m4a", "aac", "alac", "aiff", "aif", "ape", "wv", "ogg", "opus", "dsf", "dff", "cue", "m3u", "m3u8", "fpl",
];
static STARTED_BY_APP: AtomicBool = AtomicBool::new(false);
static FOOBAR_PID: AtomicU32 = AtomicU32::new(0);
static FOOBAR_SHOWN: AtomicBool = AtomicBool::new(false);
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn foobar_running() -> bool {
    Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq foobar2000.exe", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains("foobar2000.exe"))
        .unwrap_or(false)
}

// foobar2000 forwards command-line switches to the running instance
fn foobar(args: &[&str]) {
    let _ = Command::new(FOOBAR).args(args).creation_flags(CREATE_NO_WINDOW).spawn();
    keep_hidden(3000);
}

// titled top-level windows of foobar2000 (its main window)
fn foobar_windows(pid: u32, only_visible: bool) -> Vec<HWND> {
    struct Ctx {
        pid: u32,
        only_visible: bool,
        out: Vec<HWND>,
    }
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let ctx = &mut *(l as *mut Ctx);
        let mut p = 0u32;
        GetWindowThreadProcessId(h, &mut p);
        if p == ctx.pid && GetWindowTextLengthW(h) > 0 && (!ctx.only_visible || IsWindowVisible(h) != 0) {
            ctx.out.push(h);
        }
        1
    }
    let mut ctx = Ctx { pid, only_visible, out: Vec::new() };
    unsafe { EnumWindows(Some(cb), &mut ctx as *mut Ctx as LPARAM) };
    ctx.out
}

// only the main window title starts with "foobar2000"; dialogs (Preferences, Save) stay visible
fn hide_main_window(pid: u32) {
    for h in foobar_windows(pid, true) {
        let mut buf = [0u16; 64];
        let n = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetWindowTextW(h, buf.as_mut_ptr(), 64) };
        let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        if title.starts_with("foobar2000") {
            unsafe { ShowWindow(h, SW_HIDE) };
        }
    }
}

fn set_foobar_visible(show: bool) {
    let pid = FOOBAR_PID.load(Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    if show {
        for h in foobar_windows(pid, false) {
            unsafe {
                ShowWindow(h, SW_RESTORE);
                SetForegroundWindow(h);
            }
        }
    } else {
        hide_main_window(pid);
    }
    FOOBAR_SHOWN.store(show, Ordering::SeqCst);
}

// foobar2000 may pop its window up when it receives files: keep it hidden for a while
fn keep_hidden(ms: u64) {
    if !STARTED_BY_APP.load(Ordering::SeqCst) {
        return;
    }
    thread::spawn(move || {
        for _ in 0..(ms / 150) {
            if !FOOBAR_SHOWN.load(Ordering::SeqCst) {
                hide_main_window(FOOBAR_PID.load(Ordering::SeqCst));
            }
            thread::sleep(Duration::from_millis(150));
        }
    });
}

fn files_from<I: IntoIterator<Item = String>>(args: I) -> Vec<String> {
    args.into_iter()
        .filter(|a| !a.starts_with('/') && !a.starts_with('-') && Path::new(a).exists())
        .collect()
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

// "Open with > Lyra Turntable" for audio files, per user (HKCU). Never touches the default app for any type.
const APP_NAME: &str = "Lyra Turntable";
fn register_open_with() {
    let exe_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let exe = exe_path.to_string_lossy().to_string();
    let exe_file = exe_path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| "lyra.exe".into());
    let reg = |args: &[&str]| {
        let _ = Command::new("reg").args(args).creation_flags(CREATE_NO_WINDOW).output();
    };
    let key = r"HKCU\Software\Classes\Lyra.Audio";
    let cmd_key = format!(r"{key}\shell\open\command");
    let current = Command::new("reg")
        .args(["query", &cmd_key, "/ve"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    if current.contains(&exe) {
        return; // already registered for this exe
    }
    // first run of this exe: remove what the app registered while it was still called "Turntable"
    reg(&["delete", r"HKCU\Software\Classes\Turntable.Audio", "/f"]);
    reg(&["delete", r"HKCU\Software\Classes\Applications\turntable.exe", "/f"]);
    for ext in AUDIO_EXT {
        reg(&["delete", &format!(r"HKCU\Software\Classes\.{ext}\OpenWithProgids"), "/v", "Turntable.Audio", "/f"]);
    }

    let cmd = format!("\"{exe}\" \"%1\"");
    let icon = format!("\"{exe}\",0");
    reg(&["add", key, "/ve", "/d", APP_NAME, "/f"]);
    reg(&["add", key, "/v", "FriendlyTypeName", "/d", "Audio", "/f"]);
    reg(&["add", &format!(r"{key}\DefaultIcon"), "/ve", "/d", &icon, "/f"]);
    reg(&["add", &cmd_key, "/ve", "/d", &cmd, "/f"]);
    let app_key = format!(r"HKCU\Software\Classes\Applications\{exe_file}");
    reg(&["add", &app_key, "/v", "FriendlyAppName", "/d", APP_NAME, "/f"]);
    reg(&["add", &format!(r"{app_key}\DefaultIcon"), "/ve", "/d", &icon, "/f"]);
    reg(&["add", &format!(r"{app_key}\shell\open\command"), "/ve", "/d", &cmd, "/f"]);
    for ext in AUDIO_EXT {
        reg(&["add", &format!(r"HKCU\Software\Classes\.{ext}\OpenWithProgids"), "/v", "Lyra.Audio", "/t", "REG_NONE", "/f"]);
        reg(&["add", &format!(r"{app_key}\SupportedTypes"), "/v", &format!(".{ext}"), "/d", "", "/f"]);
    }
}

#[tauri::command]
fn fb_add(paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    let mut args: Vec<&str> = vec!["/immediate", "/add"];
    args.extend(paths.iter().map(|s| s.as_str()));
    foobar(&args);
}

#[tauri::command]
fn fb_command(name: String) {
    let a = format!("/command:{name}");
    foobar(&[&a]);
}

#[tauri::command]
fn take_pending() -> Vec<String> {
    std::mem::take(&mut *PENDING.lock().unwrap())
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    *PENDING.lock().unwrap() = files_from(std::env::args().skip(1));

    let app = tauri::Builder::default()
        // must be the first plugin: a second launch (e.g. Open with) hands its files to this window
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            let files = files_from(argv.into_iter().skip(1));
            if !files.is_empty() {
                let _ = app.emit("open-files", files);
            }
            show_main(app);
        }))
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(engine::EngineState::default())
        .manage(library::LibraryState::default())
        .invoke_handler(tauri::generate_handler![
            // foobar2000 backend (unchanged)
            fb_add,
            fb_command,
            take_pending,
            quit_app,
            // native engine control + events
            volume::endpoint_volume_get,
            volume::endpoint_volume_set,
            engine::engine_start,
            engine::engine_send,
            engine::engine_shutdown,
            engine::engine_running,
            engine::devices,
            // app settings
            settings::settings_get,
            settings::settings_set,
            // playlist / tag / artwork (P4.2 wiring pending)
            library::playlists_list,
            library::playlists_create,
            library::playlists_items,
            library::playlists_add,
            library::playlists_remove,
            library::playlists_reorder,
            library::playlists_clear,
            library::playlists_import_m3u,
            library::playlists_export_m3u,
            library::tags_read,
            library::artwork_read,
        ])
        .setup(|app| {
            thread::spawn(register_open_with);
            // Open-with registration is backend-agnostic (it just launches Lyra). The
            // native engine is the default and only path a fresh user ever lands on
            // (P8.4): Lyra never starts or requires foobar2000. foobar2000 is launched
            // ONLY when the hidden legacy backend is explicitly selected AND it is
            // actually installed — never on the default/native path, and never two
            // players at once.
            let native = settings::load(app.handle()).backend == "native";
            if native {
                // First native startup: one-time, read-only import of foobar2000 v2
                // playlists if that profile happens to exist. Off the startup thread so
                // a slow or failed migration can never block the window from opening.
                let handle = app.handle().clone();
                thread::spawn(move || library::migrate_foobar_once(&handle));
            } else if !foobar_running() {
                // Legacy backend selected. Only launch foobar2000 if it is installed;
                // if not, fail gracefully (do not crash, do not silently launch) — the
                // UI's foobar backend will simply stay disconnected.
                if Path::new(FOOBAR).exists() {
                    if let Ok(child) = Command::new(FOOBAR).arg("/hide").creation_flags(CREATE_NO_WINDOW).spawn() {
                        STARTED_BY_APP.store(true, Ordering::SeqCst);
                        FOOBAR_PID.store(child.id(), Ordering::SeqCst);
                        keep_hidden(8000); // its window appears a moment after launch
                    }
                } else {
                    eprintln!("legacy foobar2000 backend selected but foobar2000 is not installed at {FOOBAR}; staying on the native-capable shell");
                }
            }

            let started = STARTED_BY_APP.load(Ordering::SeqCst);
            // The tray transport items drive foobar2000 (and would start it on demand),
            // so disable them in native mode — the window's own transport controls the engine.
            let fb_transport = !native;
            let play = MenuItem::with_id(app, "play", "Play / Pause", fb_transport, None::<&str>)?;
            let next = MenuItem::with_id(app, "next", "Next", fb_transport, None::<&str>)?;
            let prev = MenuItem::with_id(app, "prev", "Previous", fb_transport, None::<&str>)?;
            let sep1 = PredefinedMenuItem::separator(app)?;
            let show = MenuItem::with_id(app, "show", "Show Lyra", true, None::<&str>)?;
            let fb = MenuItem::with_id(app, "foobar", "Show / hide foobar2000 window", started, None::<&str>)?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let quit_label = if started { "Exit (also closes foobar2000)" } else { "Exit" };
            let quit = MenuItem::with_id(app, "quit", quit_label, true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&play, &next, &prev, &sep1, &show, &fb, &sep2, &quit])?;

            TrayIconBuilder::with_id("tray")
                .icon(app.default_window_icon().cloned().expect("window icon"))
                .tooltip("Lyra Turntable")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, e| match e.id.as_ref() {
                    "play" => foobar(&["/playpause"]),
                    "next" => foobar(&["/next"]),
                    "prev" => foobar(&["/prev"]),
                    "show" => show_main(app),
                    "foobar" => set_foobar_visible(!FOOBAR_SHOWN.load(Ordering::SeqCst)),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, e| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the app");

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            // Release the audio device and join the event thread before we go.
            engine::shutdown_on_exit(app);
            if STARTED_BY_APP.load(Ordering::SeqCst) {
                let _ = Command::new(FOOBAR).arg("/exit").creation_flags(CREATE_NO_WINDOW).status();
            }
        }
    });
}
