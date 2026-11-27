//! Playlist / tag / artwork commands for the native backend (P4.3 wiring).
//!
//! These are the command *surface* the React `NativeBackend` (`src/backend/
//! NativeBackend.ts`) calls. The real bodies live in `lyra-library` (its SQLite
//! [`Playlists`] store, `read_tags` and `artwork`), merged in P4.2. P4.1 defined
//! the DTO shapes below so they match the TypeScript `PlayerBackend` (`Track`,
//! `Playlist`); the wiring here keeps those JSON shapes stable.
//!
//! # Storage
//! The playlist database lives at `playlists.sqlite` under the same Tauri app
//! config dir `settings.json` uses. It is opened lazily on first use and kept in
//! Tauri-managed [`LibraryState`] behind a `Mutex`, mirroring the engine's state.
//!
//! # Robustness
//! Every command returns `Result<_, String>` instead of panicking: a database or
//! file error surfaces to the webview as a rejected promise. `tags_read` and
//! `artwork_read` never fail the whole batch for one bad file — they skip it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine as _;
use lyra_core::TrackRef;
use lyra_library::{artwork, read_tags, Playlists};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

/// Matches the TypeScript `Playlist` type in `src/backend/PlayerBackend.ts`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDto {
    /// Stable playlist id (the store's integer primary key, stringified).
    pub id: String,
    /// Display title.
    pub title: String,
    /// `true` if this is the active/selected playlist.
    pub is_current: bool,
    /// Number of items.
    pub item_count: usize,
}

/// Matches the TypeScript `Track` type in `src/backend/PlayerBackend.ts`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackDto {
    /// Track title.
    pub title: String,
    /// Artist.
    pub artist: String,
    /// Album.
    pub album: String,
    /// Length in seconds.
    pub length: f64,
    /// Stable per-item key: the source file path (the webview needs it to build
    /// the engine play queue).
    pub key: String,
}

/// Tags read from one file, keyed path → `(key, value)` pairs.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagsDto {
    /// Source file path these tags came from.
    pub path: String,
    /// Raw tag key/value pairs in the source container's naming.
    pub tags: Vec<(String, String)>,
}

/// Tauri-managed handle to the playlist store, opened lazily on first use.
#[derive(Default)]
pub struct LibraryState(Mutex<Option<Playlists>>);

/// Path to the playlist database, creating the config dir if needed.
fn db_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("playlists.sqlite"))
}

/// Run `f` against the playlist store, opening it on first use. The store stays
/// locked for the duration of `f`, so commands never race on the connection.
fn with_store<T>(
    app: &AppHandle,
    state: &LibraryState,
    f: impl FnOnce(&mut Playlists) -> Result<T, String>,
) -> Result<T, String> {
    let mut guard = state.0.lock().unwrap();
    if guard.is_none() {
        let path = db_path(app)?;
        *guard = Some(Playlists::open(&path).map_err(|e| e.to_string())?);
    }
    f(guard.as_mut().expect("store just opened"))
}

/// Parse a webview playlist id string back into the store's integer key.
fn parse_id(id: &str) -> Result<i64, String> {
    id.parse::<i64>().map_err(|_| format!("invalid playlist id: {id}"))
}

/// Final path component, used as a title fallback when a file has no tags.
fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_string()
}

/// Resolve one playlist item to its display row via `read_tags`, falling back to
/// the file name when the file is missing or untaggable (never fails).
fn track_dto(item: &TrackRef) -> TrackDto {
    let path = item.path.clone();
    let (title, artist, album, length) = match read_tags(Path::new(&path)) {
        Ok(m) => (
            m.title.unwrap_or_else(|| file_stem(&path)),
            m.artist.unwrap_or_default(),
            m.album.unwrap_or_default(),
            m.duration.map(|d| d.as_secs_f64()).unwrap_or(0.0),
        ),
        Err(_) => (file_stem(&path), String::new(), String::new(), 0.0),
    };
    TrackDto { title, artist, album, length, key: path }
}

/// One-time import of foobar2000 v2 playlists into the native library (P8.4).
///
/// Runs on first native startup. If a foobar2000 v2 profile exists under
/// `%APPDATA%\foobar2000-v2\playlists-v2.0\`, its playlists are imported once,
/// then a persisted guard (`AppSettings::foobar_migrated`) is set so it never
/// re-runs and never clobbers playlists the user has since edited. The foobar2000
/// profile is only ever read (never written — RUNBOOK forbidden). If no profile
/// exists this is a no-op and the guard stays unset, so a later foobar2000 install
/// can still be imported once. Any failure is swallowed so startup is never blocked.
pub fn migrate_foobar_once(app: &AppHandle) {
    if crate::settings::load(app).foobar_migrated {
        return; // already done: never re-run, never clobber user playlists
    }
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return; // no %APPDATA%: nothing to migrate from
    };
    let profile = Path::new(&appdata).join("foobar2000-v2");
    if !profile.join("playlists-v2.0").join("index.txt").exists() {
        return; // foobar2000 v2 not installed / no playlists: no-op, leave guard unset
    }

    let state = app.state::<LibraryState>();
    let outcome = with_store(app, &state, |store| {
        store.migrate_foobar_playlists(&profile).map_err(|e| e.to_string())
    });
    match outcome {
        Ok(report) => {
            for (title, result) in &report {
                match result {
                    Ok(s) => eprintln!("foobar migrate: {title:?} — {} imported, {} skipped", s.imported, s.skipped),
                    Err(e) => eprintln!("foobar migrate: {title:?} — skipped ({e})"),
                }
            }
            // Mark done even if individual playlists failed: we attempted the
            // migration against a real profile, so it must not run again.
            let mut settings = crate::settings::load(app);
            settings.foobar_migrated = true;
            if let Err(e) = crate::settings::persist(app, &settings) {
                eprintln!("foobar migrate: could not persist guard ({e}); may retry next start");
            }
        }
        // Could not even read the index: leave the guard unset so a transient
        // failure (e.g. a locked profile) can retry on a later start.
        Err(e) => eprintln!("foobar migrate: aborted ({e}); will retry next start"),
    }
}

/// List all playlists with their item counts.
#[tauri::command]
pub fn playlists_list(app: AppHandle, state: State<'_, LibraryState>) -> Result<Vec<PlaylistDto>, String> {
    with_store(&app, &state, |store| {
        let playlists = store.list().map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(playlists.len());
        for p in playlists {
            let item_count = store.item_count(p.id).map_err(|e| e.to_string())?;
            out.push(PlaylistDto {
                id: p.id.to_string(),
                title: p.title,
                is_current: p.is_current,
                item_count,
            });
        }
        Ok(out)
    })
}

/// Create an empty playlist, returning its new id. It does not become current:
/// the webview owns the "selected playlist" notion (native adds target an
/// explicit id, so no persisted current is needed).
#[tauri::command]
pub fn playlists_create(app: AppHandle, state: State<'_, LibraryState>, title: String) -> Result<String, String> {
    with_store(&app, &state, |store| store.create(&title, false).map(|id| id.to_string()).map_err(|e| e.to_string()))
}

/// List the items of a playlist, each resolved to a display row via `read_tags`.
#[tauri::command]
pub fn playlists_items(app: AppHandle, state: State<'_, LibraryState>, id: String) -> Result<Vec<TrackDto>, String> {
    let pid = parse_id(&id)?;
    with_store(&app, &state, |store| {
        let items = store.items(pid).map_err(|e| e.to_string())?;
        Ok(items.iter().map(track_dto).collect())
    })
}

/// Append file paths to a playlist (each becomes a whole-file item).
#[tauri::command]
pub fn playlists_add(app: AppHandle, state: State<'_, LibraryState>, id: String, paths: Vec<String>) -> Result<(), String> {
    let pid = parse_id(&id)?;
    let items: Vec<TrackRef> = paths.into_iter().map(TrackRef::file).collect();
    with_store(&app, &state, |store| store.add_items(pid, &items).map_err(|e| e.to_string()))
}

/// Remove items at the given indices from a playlist.
#[tauri::command]
pub fn playlists_remove(app: AppHandle, state: State<'_, LibraryState>, id: String, indices: Vec<usize>) -> Result<(), String> {
    let pid = parse_id(&id)?;
    with_store(&app, &state, |store| store.remove_items(pid, &indices).map_err(|e| e.to_string()))
}

/// Move the item at `from` to index `to` (drag-to-reorder in the playlist card).
#[tauri::command]
pub fn playlists_reorder(app: AppHandle, state: State<'_, LibraryState>, id: String, from: usize, to: usize) -> Result<(), String> {
    let pid = parse_id(&id)?;
    with_store(&app, &state, |store| store.reorder(pid, from, to).map_err(|e| e.to_string()))
}

/// Clear all items from a playlist.
#[tauri::command]
pub fn playlists_clear(app: AppHandle, state: State<'_, LibraryState>, id: String) -> Result<(), String> {
    let pid = parse_id(&id)?;
    with_store(&app, &state, |store| store.clear(pid).map_err(|e| e.to_string()))
}

/// Import an m3u/m3u8 file as a new playlist, returning its new id.
#[tauri::command]
pub fn playlists_import_m3u(app: AppHandle, state: State<'_, LibraryState>, path: String) -> Result<String, String> {
    with_store(&app, &state, |store| store.import_m3u(Path::new(&path)).map(|id| id.to_string()).map_err(|e| e.to_string()))
}

/// Export a playlist to an m3u file at `path`.
#[tauri::command]
pub fn playlists_export_m3u(app: AppHandle, state: State<'_, LibraryState>, id: String, path: String) -> Result<(), String> {
    let pid = parse_id(&id)?;
    with_store(&app, &state, |store| store.export_m3u(pid, Path::new(&path)).map_err(|e| e.to_string()))
}

/// Read tags for a batch of files. Files that cannot be read are skipped (they
/// never fail the whole batch), so the result may be shorter than `paths`.
#[tauri::command]
pub fn tags_read(paths: Vec<String>) -> Vec<TagsDto> {
    paths
        .into_iter()
        .filter_map(|path| {
            let meta = read_tags(Path::new(&path)).ok()?;
            let tags = meta.tags.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            Some(TagsDto { path, tags })
        })
        .collect()
}

/// Read embedded (or folder) cover art for a file, returned as a base64
/// `data:` URL, or `None` when there is no art or the file cannot be read.
#[tauri::command]
pub fn artwork_read(path: String) -> Option<String> {
    let art = artwork(Path::new(&path)).ok()??;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&art.data);
    Some(format!("data:{};base64,{b64}", art.mime))
}
