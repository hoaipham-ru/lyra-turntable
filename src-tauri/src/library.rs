//! Playlist / tag / artwork commands for the native backend (P4.1 signatures).
//!
//! These are the command *surface* the React `NativeBackend` (P4.3) will call.
//! The real bodies live in `lyra-library`, whose Playlists store, `read_tags` and
//! `artwork` are being added in parallel by P4.2. To keep this task compiling
//! standalone, every command here is a compile-safe stub returning an empty
//! result. The orchestrator merges P4.2 before P4.3, so when P4.3 lands these can
//! be wired to the real API.
//!
//! TODO(P4.2 wiring): replace each stub body with a call into `lyra_library`
//! (Playlists store, `read_tags`, `artwork`). The DTO shapes below already match
//! the TypeScript `PlayerBackend` (`Track`, `Playlist`) so the wiring is a body
//! change only — the command signatures and JSON shapes stay stable for P4.3.

use serde::Serialize;

/// Matches the TypeScript `Playlist` type in `src/backend/PlayerBackend.ts`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDto {
    /// Stable playlist id.
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
    /// Stable per-item key (e.g. source path).
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

/// List all playlists.
#[tauri::command]
pub fn playlists_list() -> Vec<PlaylistDto> {
    // TODO(P4.2 wiring): lyra_library Playlists store.
    Vec::new()
}

/// Create a playlist, returning its new id.
#[tauri::command]
pub fn playlists_create(_title: String) -> String {
    // TODO(P4.2 wiring): lyra_library Playlists store.
    String::new()
}

/// List the items of a playlist.
#[tauri::command]
pub fn playlists_items(_id: String) -> Vec<TrackDto> {
    // TODO(P4.2 wiring): lyra_library Playlists store.
    Vec::new()
}

/// Append file paths to a playlist.
#[tauri::command]
pub fn playlists_add(_id: String, _paths: Vec<String>) {
    // TODO(P4.2 wiring): lyra_library Playlists store.
}

/// Remove items at the given indices from a playlist.
#[tauri::command]
pub fn playlists_remove(_id: String, _indices: Vec<usize>) {
    // TODO(P4.2 wiring): lyra_library Playlists store.
}

/// Clear all items from a playlist.
#[tauri::command]
pub fn playlists_clear(_id: String) {
    // TODO(P4.2 wiring): lyra_library Playlists store.
}

/// Import an m3u/m3u8 file as a new playlist, returning its new id.
#[tauri::command]
pub fn playlists_import_m3u(_path: String) -> String {
    // TODO(P4.2 wiring): lyra_library m3u import.
    String::new()
}

/// Export a playlist to an m3u file at `path`.
#[tauri::command]
pub fn playlists_export_m3u(_id: String, _path: String) {
    // TODO(P4.2 wiring): lyra_library m3u export.
}

/// Read tags for a batch of files.
#[tauri::command]
pub fn tags_read(_paths: Vec<String>) -> Vec<TagsDto> {
    // TODO(P4.2 wiring): lyra_library read_tags.
    Vec::new()
}

/// Read embedded artwork for a file, returned as a base64 data URL, or `None`.
#[tauri::command]
pub fn artwork_read(_path: String) -> Option<String> {
    // TODO(P4.2 wiring): lyra_library artwork.
    None
}
