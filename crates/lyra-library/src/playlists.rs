//! Playlist store and `m3u`/`m3u8` import/export (S06, P4.2).
//!
//! [`Playlists`] is a small SQLite-backed store (via `rusqlite`, bundled SQLite so
//! no system library is needed) for the app alpha's playlists. It holds an ordered
//! list of playlists, each with an ordered list of items (a path plus an optional
//! cue sub-track index, mirroring [`TrackRef`]).
//!
//! # Schema
//! ```text
//! playlists(id, title, is_current, position)
//! playlist_items(playlist_id, position, path, subtrack)
//! ```
//! Item positions are kept contiguous and 0-based so [`Playlists::items`] returns
//! them in playback order and reorders stay stable.
//!
//! # Default Playlist
//! foobar2000's "Default Playlist" is emptied at startup unless the user has
//! touched it or it is playing. That *policy* lives in the UI; this crate exposes
//! the primitives it needs — [`Playlists::ensure_default`] (idempotent: returns the
//! Default Playlist's id and current item count, creating it if missing) and
//! [`Playlists::clear`] (empties a playlist).
//!
//! # Formats
//! `.m3u`/`.m3u8` import and export are fully supported (UTF-8, optional `#EXTINF`
//! lines ignored on import, relative paths resolved against the playlist file's
//! directory).
//!
//! ## foobar2000 import (P6.3)
//! foobar2000 **v2** keeps its playlists under
//! `%APPDATA%\foobar2000-v2\playlists-v2.0\` as `playlist-<GUID>.fplite` files.
//! Inspecting the real profile (read-only) showed these are **not** SQLite
//! databases as first assumed, but a trivial UTF-8 text format:
//!
//! ```text
//! <BOM>file://C:\Users\me\Music\track1.mp3\n
//! file://C:\Users\me\Music\track2.flac\n
//! ...
//! ```
//!
//! i.e. an optional UTF-8 BOM followed by one location string per LF-terminated
//! line, in playback order. The sibling `playlist-<GUID>-props.sqlite` *is* real
//! SQLite but holds only per-playlist properties (GUID-keyed blobs, focus marker),
//! not the track list, so it is ignored. `index.txt` maps `GUID:title` in playlist
//! order and drives [`Playlists::migrate_foobar_playlists`].
//!
//! [`Playlists::import_fplite`] parses that text format (best-effort: unresolvable
//! lines are counted and skipped, never fatal). The classic binary `.fpl`
//! (foobar2000 v1.x) is undocumented; [`Playlists::import_fpl`] is a genuine
//! best-effort scan for embedded `file://` UTF-8 location strings. Both report an
//! [`ImportSummary`] (imported vs skipped). If an `.fplite` is ever found to carry
//! the SQLite magic instead of text, it is reported as
//! [`LyraError::UnsupportedFormat`] rather than guessed.

use std::path::Path;

use rusqlite::{params, Connection};

use lyra_core::{LyraError, Result, TrackRef};

/// The reserved title of the Default Playlist.
pub const DEFAULT_PLAYLIST_TITLE: &str = "Default Playlist";

/// Title given to foobar2000's own scratch playlist when it is imported, so it
/// never collides with Lyra's [`DEFAULT_PLAYLIST_TITLE`] (two identical names in
/// the picker, 2026-11-27).
pub const IMPORTED_DEFAULT_TITLE: &str = "Default Playlist (foobar2000)";

/// foobar2000 names its scratch playlist "Default" (older) or "Default Playlist".
fn is_default_title(title: &str) -> bool {
    let t = title.trim().to_ascii_lowercase();
    t == "default" || t == "default playlist"
}

/// A playlist header row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    /// Stable primary key.
    pub id: i64,
    /// Display title.
    pub title: String,
    /// Whether this is the currently active playlist.
    pub is_current: bool,
    /// Ordering position among playlists (0-based, contiguous).
    pub position: i64,
}

/// Outcome of a best-effort foobar2000 playlist import.
///
/// `imported` counts the location strings that became playlist items (in order);
/// `skipped` counts lines/segments that could not be resolved to a location and
/// were tolerated rather than aborting the whole import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    /// Id of the created Lyra playlist.
    pub playlist_id: i64,
    /// Number of items added, in stored order.
    pub imported: usize,
    /// Number of unresolvable entries that were skipped.
    pub skipped: usize,
}

/// A SQLite-backed playlist store.
pub struct Playlists {
    conn: Connection,
}

impl Playlists {
    /// Open (creating if necessary) the playlist database at `path`.
    ///
    /// The path `":memory:"` opens a private in-memory database (used by tests).
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] if the database cannot be opened or its schema
    /// cannot be created.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = if path == Path::new(":memory:") {
            Connection::open_in_memory()
        } else {
            Connection::open(path)
        }
        .map_err(|e| db_err("open database", &e))?;
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    /// Create the schema and enable foreign-key enforcement.
    fn init_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE IF NOT EXISTS playlists (
                     id         INTEGER PRIMARY KEY AUTOINCREMENT,
                     title      TEXT    NOT NULL,
                     is_current INTEGER NOT NULL DEFAULT 0,
                     position   INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS playlist_items (
                     playlist_id INTEGER NOT NULL,
                     position    INTEGER NOT NULL,
                     path        TEXT    NOT NULL,
                     subtrack    INTEGER,
                     PRIMARY KEY (playlist_id, position),
                     FOREIGN KEY (playlist_id) REFERENCES playlists(id) ON DELETE CASCADE
                 );",
            )
            .map_err(|e| db_err("create schema", &e))
    }

    /// List all playlists in position order.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn list(&self) -> Result<Vec<Playlist>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, title, is_current, position FROM playlists ORDER BY position, id")
            .map_err(|e| db_err("list playlists", &e))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Playlist {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    is_current: row.get::<_, i64>(2)? != 0,
                    position: row.get(3)?,
                })
            })
            .map_err(|e| db_err("list playlists", &e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_err("list playlists", &e))
    }

    /// Create a new, empty playlist with `title`, appended after existing ones.
    /// If `set_current` is true it also becomes the current playlist. Returns the
    /// new playlist's id.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn create(&mut self, title: &str, set_current: bool) -> Result<i64> {
        let next_pos: i64 = self
            .conn
            .query_row("SELECT COALESCE(MAX(position) + 1, 0) FROM playlists", [], |r| r.get(0))
            .map_err(|e| db_err("create playlist", &e))?;
        self.conn
            .execute(
                "INSERT INTO playlists (title, is_current, position) VALUES (?1, 0, ?2)",
                params![title, next_pos],
            )
            .map_err(|e| db_err("create playlist", &e))?;
        let id = self.conn.last_insert_rowid();
        if set_current {
            self.set_current(id)?;
        }
        Ok(id)
    }

    /// Remove a playlist and all of its items.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn remove(&mut self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM playlists WHERE id = ?1", params![id])
            .map_err(|e| db_err("remove playlist", &e))?;
        Ok(())
    }

    /// Rename a playlist.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn rename(&mut self, id: i64, title: &str) -> Result<()> {
        self.conn
            .execute("UPDATE playlists SET title = ?2 WHERE id = ?1", params![id, title])
            .map_err(|e| db_err("rename playlist", &e))?;
        Ok(())
    }

    /// Make `id` the current playlist (clearing the flag on all others).
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error, or
    /// [`LyraError::InvalidState`] if `id` does not exist.
    pub fn set_current(&mut self, id: i64) -> Result<()> {
        let tx = self.conn.transaction().map_err(|e| db_err("set current", &e))?;
        let exists: bool = tx
            .query_row("SELECT 1 FROM playlists WHERE id = ?1", params![id], |_| Ok(()))
            .is_ok();
        if !exists {
            return Err(LyraError::InvalidState(format!("no playlist with id {id}")));
        }
        tx.execute("UPDATE playlists SET is_current = 0", [])
            .map_err(|e| db_err("set current", &e))?;
        tx.execute("UPDATE playlists SET is_current = 1 WHERE id = ?1", params![id])
            .map_err(|e| db_err("set current", &e))?;
        tx.commit().map_err(|e| db_err("set current", &e))
    }

    /// The id of the current playlist, if one is set.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn current(&self) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT id FROM playlists WHERE is_current = 1 ORDER BY position, id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(db_err("current playlist", &other)),
            })
    }

    /// Ensure the Default Playlist exists, returning `(id, item_count)`.
    ///
    /// Idempotent: calling it repeatedly returns the same id and never duplicates
    /// the playlist. It does not empty the playlist — the startup-clear policy lives
    /// in the UI, which calls [`clear`](Self::clear) when its rule says to.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn ensure_default(&mut self) -> Result<(i64, usize)> {
        self.dedupe_default_titles()?;
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM playlists WHERE title = ?1 ORDER BY position, id LIMIT 1",
                params![DEFAULT_PLAYLIST_TITLE],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(db_err("ensure default", &other)),
            })?;
        let id = match existing {
            Some(id) => id,
            None => self.create(DEFAULT_PLAYLIST_TITLE, false)?,
        };
        let count = self.item_count(id)?;
        Ok((id, count))
    }

    /// Keep exactly one playlist titled [`DEFAULT_PLAYLIST_TITLE`]: the first one
    /// (by position, id) keeps it, any later ones are renamed to
    /// "Default Playlist (2)", "(3)", ... Returns how many were renamed.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn dedupe_default_titles(&mut self) -> Result<usize> {
        let ids: Vec<i64> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM playlists WHERE title = ?1 ORDER BY position, id")
                .map_err(|e| db_err("dedupe default", &e))?;
            let rows = stmt
                .query_map(params![DEFAULT_PLAYLIST_TITLE], |r| r.get(0))
                .map_err(|e| db_err("dedupe default", &e))?;
            rows.collect::<std::result::Result<_, _>>()
                .map_err(|e| db_err("dedupe default", &e))?
        };
        let mut renamed = 0;
        for (n, id) in ids.iter().skip(1).enumerate() {
            // Neutral name: an existing store cannot tell which copy came from foobar2000.
            let title = format!("{DEFAULT_PLAYLIST_TITLE} ({})", n + 2);
            self.rename(*id, &title)?;
            renamed += 1;
        }
        Ok(renamed)
    }

    /// The items of a playlist, in playback order.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn items(&self, id: i64) -> Result<Vec<TrackRef>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, subtrack FROM playlist_items WHERE playlist_id = ?1 ORDER BY position")
            .map_err(|e| db_err("read items", &e))?;
        let rows = stmt
            .query_map(params![id], |row| {
                let path: String = row.get(0)?;
                let subtrack: Option<i64> = row.get(1)?;
                Ok(TrackRef {
                    path,
                    subtrack: subtrack.map(|s| s as u32),
                })
            })
            .map_err(|e| db_err("read items", &e))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_err("read items", &e))
    }

    /// Number of items in a playlist.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn item_count(&self, id: i64) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM playlist_items WHERE playlist_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(|e| db_err("count items", &e))?;
        Ok(n as usize)
    }

    /// Append items to the end of a playlist.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn add_items(&mut self, id: i64, items: &[TrackRef]) -> Result<()> {
        let mut next: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_items WHERE playlist_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(|e| db_err("add items", &e))?;
        let tx = self.conn.transaction().map_err(|e| db_err("add items", &e))?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO playlist_items (playlist_id, position, path, subtrack) VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(|e| db_err("add items", &e))?;
            for item in items {
                stmt.execute(params![id, next, item.path, item.subtrack.map(i64::from)])
                    .map_err(|e| db_err("add items", &e))?;
                next += 1;
            }
        }
        tx.commit().map_err(|e| db_err("add items", &e))
    }

    /// Remove items at the given positions (0-based indices into the current
    /// order), then renumber the rest to stay contiguous. Out-of-range indices are
    /// ignored.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn remove_items(&mut self, id: i64, indices: &[usize]) -> Result<()> {
        let mut items = self.items(id)?;
        let drop: std::collections::HashSet<usize> = indices.iter().copied().collect();
        let kept: Vec<TrackRef> = items
            .drain(..)
            .enumerate()
            .filter(|(i, _)| !drop.contains(i))
            .map(|(_, t)| t)
            .collect();
        self.rewrite_items(id, &kept)
    }

    /// Remove all items from a playlist (keeps the playlist itself).
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn clear(&mut self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM playlist_items WHERE playlist_id = ?1", params![id])
            .map_err(|e| db_err("clear playlist", &e))?;
        Ok(())
    }

    /// Move the item at index `from` to index `to`, shifting the others. Indices
    /// out of range are clamped to the valid range.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn reorder(&mut self, id: i64, from: usize, to: usize) -> Result<()> {
        let mut items = self.items(id)?;
        if items.is_empty() {
            return Ok(());
        }
        let from = from.min(items.len() - 1);
        let to = to.min(items.len() - 1);
        if from == to {
            return Ok(());
        }
        let item = items.remove(from);
        items.insert(to, item);
        self.rewrite_items(id, &items)
    }

    /// Replace a playlist's items wholesale with `items`, renumbered 0-based.
    fn rewrite_items(&mut self, id: i64, items: &[TrackRef]) -> Result<()> {
        let tx = self.conn.transaction().map_err(|e| db_err("rewrite items", &e))?;
        tx.execute("DELETE FROM playlist_items WHERE playlist_id = ?1", params![id])
            .map_err(|e| db_err("rewrite items", &e))?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO playlist_items (playlist_id, position, path, subtrack) VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(|e| db_err("rewrite items", &e))?;
            for (pos, item) in items.iter().enumerate() {
                stmt.execute(params![id, pos as i64, item.path, item.subtrack.map(i64::from)])
                    .map_err(|e| db_err("rewrite items", &e))?;
            }
        }
        tx.commit().map_err(|e| db_err("rewrite items", &e))
    }

    /// Import an `.m3u`/`.m3u8` playlist as a new playlist named after the file,
    /// returning its id. Relative entries are resolved against the file's directory;
    /// `#`-comment lines (including `#EXTINF`) are ignored.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] if the file cannot be read or a database error
    /// occurs.
    pub fn import_m3u(&mut self, path: &Path) -> Result<i64> {
        let bytes = std::fs::read(path)
            .map_err(|e| LyraError::Io(format!("reading m3u {}: {e}", path.display())))?;
        let items = parse_m3u(&bytes, path.parent());
        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Imported Playlist")
            .to_string();
        let id = self.create(&title, false)?;
        self.add_items(id, &items)?;
        Ok(id)
    }

    /// Export a playlist to an `.m3u8` file (UTF-8, `#EXTM3U` header, one path per
    /// line). Sub-track indices are not representable in m3u and are omitted.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] if the file cannot be written or a database error
    /// occurs.
    pub fn export_m3u(&self, id: i64, path: &Path) -> Result<()> {
        let items = self.items(id)?;
        let mut out = String::from("#EXTM3U\n");
        for item in &items {
            out.push_str(&item.path);
            out.push('\n');
        }
        std::fs::write(path, out)
            .map_err(|e| LyraError::Io(format!("writing m3u {}: {e}", path.display())))
    }

    /// Import a foobar2000 playlist, dispatching on the file extension:
    /// `.fplite` (v2 text format) → [`import_fplite`](Self::import_fplite),
    /// `.fpl` (classic binary) → [`import_fpl`](Self::import_fpl). Returns the new
    /// playlist's id.
    ///
    /// # Errors
    /// Returns [`LyraError::UnsupportedFormat`] for any other extension, or the
    /// underlying importer's error.
    pub fn import_foobar_playlist(&mut self, path: &Path) -> Result<i64> {
        match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("fplite") => self.import_fplite(path),
            Some("fpl") => self.import_fpl(path),
            _ => Err(LyraError::UnsupportedFormat(format!(
                "not a foobar2000 playlist (expected .fplite or .fpl): {}",
                path.display()
            ))),
        }
    }

    /// Import a foobar2000 **v2** `.fplite` playlist, returning the new playlist id.
    ///
    /// The playlist is named after the file stem. See
    /// [`import_fplite_report`](Self::import_fplite_report) for the skipped-entry
    /// count.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] if the file cannot be read or a database error
    /// occurs, or [`LyraError::UnsupportedFormat`] if the file is a SQLite database
    /// rather than the expected text format.
    pub fn import_fplite(&mut self, path: &Path) -> Result<i64> {
        self.import_fplite_report(path).map(|s| s.playlist_id)
    }

    /// Import a `.fplite` playlist and report how many entries were imported vs
    /// skipped. Best-effort: unresolvable lines are skipped, never fatal.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] if the file cannot be read or a database error
    /// occurs, or [`LyraError::UnsupportedFormat`] if the file carries the SQLite
    /// magic instead of the expected text format.
    pub fn import_fplite_report(&mut self, path: &Path) -> Result<ImportSummary> {
        let title = fplite_title(path);
        self.import_fplite_with_title(path, &title)
    }

    /// Import a `.fplite` playlist under an explicit `title` (used by migration so
    /// the title comes from `index.txt`, not the GUID file name).
    fn import_fplite_with_title(&mut self, path: &Path, title: &str) -> Result<ImportSummary> {
        let bytes = std::fs::read(path)
            .map_err(|e| LyraError::Io(format!("reading fplite {}: {e}", path.display())))?;
        if bytes.starts_with(SQLITE_MAGIC) {
            return Err(LyraError::UnsupportedFormat(format!(
                ".fplite is a SQLite database, not the expected text format \
                 (schema unknown, not guessing): {}",
                path.display()
            )));
        }
        let (items, skipped) = parse_fplite_text(&bytes);
        self.store_import(title, items, skipped)
    }

    /// Best-effort import of a classic binary `.fpl` (foobar2000 v1.x). The format
    /// is undocumented; this scans the bytes for embedded `file://` UTF-8 location
    /// strings in order. Returns the new playlist id.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] if the file cannot be read or a database error
    /// occurs.
    pub fn import_fpl(&mut self, path: &Path) -> Result<i64> {
        self.import_fpl_report(path).map(|s| s.playlist_id)
    }

    /// Best-effort `.fpl` import, reporting imported vs skipped counts.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] if the file cannot be read or a database error
    /// occurs.
    pub fn import_fpl_report(&mut self, path: &Path) -> Result<ImportSummary> {
        let bytes = std::fs::read(path)
            .map_err(|e| LyraError::Io(format!("reading fpl {}: {e}", path.display())))?;
        let title = fplite_title(path);
        let (items, skipped) = scan_fpl_locations(&bytes);
        self.store_import(&title, items, skipped)
    }

    /// Create a playlist named `title` and append `items`, returning a summary.
    fn store_import(
        &mut self,
        title: &str,
        items: Vec<TrackRef>,
        skipped: usize,
    ) -> Result<ImportSummary> {
        let imported = items.len();
        let id = self.create(title, false)?;
        self.add_items(id, &items)?;
        Ok(ImportSummary { playlist_id: id, imported, skipped })
    }

    /// Migrate every foobar2000 **v2** playlist from a profile directory.
    ///
    /// Reads `<profile_dir>/playlists-v2.0/index.txt` (lines `GUID:title`, in
    /// playlist order) and imports each `playlist-<GUID>.fplite` in that order,
    /// naming it from `index.txt`. Returns one `(title, result)` pair per playlist
    /// so per-playlist failures are **reported, not guessed** (a missing or
    /// unparseable file fails only its own entry). The profile is only ever read.
    ///
    /// Intended to be run once by the caller; this function performs the import but
    /// keeps no "already migrated" state of its own.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] if `playlists-v2.0\index.txt` cannot be read. A
    /// per-playlist import error is carried in the returned vector, not propagated.
    pub fn migrate_foobar_playlists(
        &mut self,
        profile_dir: &Path,
    ) -> Result<Vec<(String, std::result::Result<ImportSummary, LyraError>)>> {
        let dir = profile_dir.join("playlists-v2.0");
        let index = dir.join("index.txt");
        let bytes = std::fs::read(&index)
            .map_err(|e| LyraError::Io(format!("reading {}: {e}", index.display())))?;
        let mut out = Vec::new();
        for (guid, title) in parse_index(&bytes) {
            let file = dir.join(format!("playlist-{guid}.fplite"));
            // foobar2000's scratch playlist must not become a second "Default Playlist".
            let title = if is_default_title(&title) { IMPORTED_DEFAULT_TITLE.to_string() } else { title };
            let result = if file.exists() {
                self.import_fplite_with_title(&file, &title)
            } else {
                Err(LyraError::Io(format!("missing playlist file {}", file.display())))
            };
            out.push((title, result));
        }
        Ok(out)
    }
}

/// Parse m3u/m3u8 bytes into track references.
///
/// Decodes UTF-8 lossily, strips a leading BOM, ignores blank and `#`-prefixed
/// lines, and resolves relative paths against `base_dir` when one is given.
fn parse_m3u(bytes: &[u8], base_dir: Option<&Path>) -> Vec<TrackRef> {
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let text = String::from_utf8_lossy(body);
    let mut items = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let resolved = resolve_entry(base_dir, line);
        items.push(TrackRef::file(resolved));
    }
    items
}

/// Resolve an m3u entry against the playlist's directory. Absolute paths and URLs
/// are kept verbatim.
fn resolve_entry(base_dir: Option<&Path>, entry: &str) -> String {
    let p = Path::new(entry);
    if p.is_absolute() || entry.contains("://") {
        return entry.to_string();
    }
    match base_dir {
        Some(dir) => dir.join(p).to_string_lossy().into_owned(),
        None => entry.to_string(),
    }
}

/// The first 16 bytes of every SQLite 3 database file.
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

/// A UTF-8 byte-order mark.
const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// Derive a playlist title from a `.fplite`/`.fpl` path's file stem.
fn fplite_title(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported Playlist")
        .to_string()
}

/// Parse foobar2000 v2 `.fplite` text bytes into `(items, skipped)`.
///
/// Strips a leading UTF-8 BOM, then treats each non-empty line as a location
/// string (see [`location_to_path`]). Blank lines are ignored; lines that resolve
/// to nothing (e.g. a bare `file://`) are counted as skipped.
fn parse_fplite_text(bytes: &[u8]) -> (Vec<TrackRef>, usize) {
    let body = bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes);
    let text = String::from_utf8_lossy(body);
    let mut items = Vec::new();
    let mut skipped = 0;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        match location_to_path(line) {
            Some(path) => items.push(TrackRef::file(path)),
            None => skipped += 1,
        }
    }
    (items, skipped)
}

/// Best-effort scan of a classic binary `.fpl` for `file://` location strings.
///
/// Returns `(items, skipped)` in file order. Each `file://` occurrence is read up
/// to the next control byte (`< 0x20`, e.g. NUL/newline) and decoded as UTF-8;
/// segments that are not valid UTF-8 or resolve to nothing are counted as skipped.
fn scan_fpl_locations(bytes: &[u8]) -> (Vec<TrackRef>, usize) {
    const NEEDLE: &[u8] = b"file://";
    let mut items = Vec::new();
    let mut skipped = 0;
    let mut i = 0;
    while let Some(rel) = find_subslice(&bytes[i..], NEEDLE) {
        let start = i + rel;
        let mut end = start;
        while end < bytes.len() && bytes[end] >= 0x20 {
            end += 1;
        }
        match std::str::from_utf8(&bytes[start..end]) {
            Ok(seg) => match location_to_path(seg) {
                Some(path) => items.push(TrackRef::file(path)),
                None => skipped += 1,
            },
            Err(_) => skipped += 1,
        }
        i = if end > start { end } else { start + NEEDLE.len() };
    }
    (items, skipped)
}

/// Turn a foobar2000 location string into a filesystem path, or `None` to skip.
///
/// A `file://` prefix is stripped (foobar2000 writes `file://C:\...`, so the
/// Windows path with backslashes is returned verbatim); a bare `file://` yields
/// `None`. Any other string (e.g. a `http://` stream location) is kept verbatim.
/// Sub-song indices are not represented in the v2 text format, so every item is a
/// whole-file reference.
fn location_to_path(loc: &str) -> Option<String> {
    let loc = loc.trim();
    if loc.is_empty() {
        return None;
    }
    if let Some(rest) = loc.strip_prefix("file://") {
        let rest = rest.trim();
        if rest.is_empty() {
            return None;
        }
        return Some(rest.to_string());
    }
    Some(loc.to_string())
}

/// Parse `index.txt` into `(guid, title)` pairs in file order.
///
/// Each line is `GUID:title`; the split is on the first `:` so a title may itself
/// contain colons. Blank lines are ignored.
fn parse_index(bytes: &[u8]) -> Vec<(String, String)> {
    let body = bytes.strip_prefix(UTF8_BOM).unwrap_or(bytes);
    let text = String::from_utf8_lossy(body);
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((guid, title)) = line.split_once(':') {
            out.push((guid.trim().to_string(), title.trim().to_string()));
        }
    }
    out
}

/// Find the first occurrence of `needle` in `hay`, byte-for-byte.
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Wrap a `rusqlite` error as a [`LyraError::Io`] with a context message.
fn db_err(context: &str, e: &rusqlite::Error) -> LyraError {
    LyraError::Io(format!("playlist store ({context}): {e}"))
}

#[cfg(test)]
mod default_title_tests {
    use super::*;

    #[test]
    fn duplicate_default_titles_are_renamed_and_one_default_remains() {
        let dir = std::env::temp_dir().join(format!("lyra-dedupe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("p.sqlite");
        let _ = std::fs::remove_file(&db);
        let mut store = Playlists::open(&db).unwrap();
        let a = store.create(DEFAULT_PLAYLIST_TITLE, false).unwrap();
        let b = store.create(DEFAULT_PLAYLIST_TITLE, false).unwrap();
        let (id, _) = store.ensure_default().unwrap();
        assert_eq!(id, a);
        let titles: Vec<_> = store.list().unwrap().into_iter().map(|p| (p.id, p.title)).collect();
        assert_eq!(titles.iter().filter(|(_, t)| t == DEFAULT_PLAYLIST_TITLE).count(), 1);
        assert!(titles.iter().any(|(i, t)| *i == b && t == "Default Playlist (2)"));
        assert!(is_default_title("Default") && is_default_title(" default playlist "));
        assert!(!is_default_title(IMPORTED_DEFAULT_TITLE));
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Playlists {
        Playlists::open(Path::new(":memory:")).unwrap()
    }

    fn refs(paths: &[&str]) -> Vec<TrackRef> {
        paths.iter().map(|p| TrackRef::file(*p)).collect()
    }

    #[test]
    fn crud_and_current() {
        let mut pl = mem();
        let a = pl.create("A", false).unwrap();
        let b = pl.create("B", true).unwrap();
        let list = pl.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(pl.current().unwrap(), Some(b));
        assert!(list.iter().find(|p| p.id == b).unwrap().is_current);

        pl.rename(a, "A2").unwrap();
        assert_eq!(pl.list().unwrap()[0].title, "A2");

        pl.set_current(a).unwrap();
        assert_eq!(pl.current().unwrap(), Some(a));
        assert!(!pl.list().unwrap().iter().find(|p| p.id == b).unwrap().is_current);

        pl.remove(b).unwrap();
        assert_eq!(pl.list().unwrap().len(), 1);
    }

    #[test]
    fn set_current_rejects_unknown() {
        let mut pl = mem();
        let err = pl.set_current(999).unwrap_err();
        assert!(matches!(err, LyraError::InvalidState(_)), "got {err:?}");
    }

    #[test]
    fn add_remove_reorder_items() {
        let mut pl = mem();
        let id = pl.create("P", false).unwrap();
        pl.add_items(id, &refs(&["a", "b", "c", "d"])).unwrap();
        assert_eq!(paths(&pl, id), ["a", "b", "c", "d"]);

        // Remove indices 1 and 3 ("b" and "d").
        pl.remove_items(id, &[1, 3]).unwrap();
        assert_eq!(paths(&pl, id), ["a", "c"]);

        // Append more and reorder: move index 0 to index 2.
        pl.add_items(id, &refs(&["e"])).unwrap();
        assert_eq!(paths(&pl, id), ["a", "c", "e"]);
        pl.reorder(id, 0, 2).unwrap();
        assert_eq!(paths(&pl, id), ["c", "e", "a"]);

        pl.clear(id).unwrap();
        assert_eq!(pl.item_count(id).unwrap(), 0);
    }

    #[test]
    fn subtrack_round_trips_in_store() {
        let mut pl = mem();
        let id = pl.create("P", false).unwrap();
        pl.add_items(
            id,
            &[
                TrackRef { path: "album.flac".into(), subtrack: Some(2) },
                TrackRef::file("single.flac"),
            ],
        )
        .unwrap();
        let items = pl.items(id).unwrap();
        assert_eq!(items[0].subtrack, Some(2));
        assert_eq!(items[1].subtrack, None);
    }

    #[test]
    fn ensure_default_is_idempotent_and_clear_empties() {
        let mut pl = mem();
        let (id1, count1) = pl.ensure_default().unwrap();
        assert_eq!(count1, 0);
        pl.add_items(id1, &refs(&["x", "y"])).unwrap();

        let (id2, count2) = pl.ensure_default().unwrap();
        assert_eq!(id2, id1, "ensure_default must not duplicate");
        assert_eq!(count2, 2);
        // Only one Default Playlist exists.
        let defaults = pl
            .list()
            .unwrap()
            .into_iter()
            .filter(|p| p.title == DEFAULT_PLAYLIST_TITLE)
            .count();
        assert_eq!(defaults, 1);

        pl.clear(id1).unwrap();
        assert_eq!(pl.ensure_default().unwrap().1, 0);
    }

    #[test]
    fn remove_playlist_cascades_items() {
        let mut pl = mem();
        let id = pl.create("P", false).unwrap();
        pl.add_items(id, &refs(&["a", "b"])).unwrap();
        pl.remove(id).unwrap();
        // The items table no longer has rows for the removed playlist.
        assert_eq!(pl.item_count(id).unwrap(), 0);
    }

    /// Build a `.fplite` byte blob: optional BOM + `file://`/raw lines (LF).
    fn fplite_bytes(with_bom: bool, lines: &[&str]) -> Vec<u8> {
        let mut v = Vec::new();
        if with_bom {
            v.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        for l in lines {
            v.extend_from_slice(l.as_bytes());
            v.push(b'\n');
        }
        v
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lyra_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fplite_imports_in_order_and_strips_file_scheme() {
        let dir = tmp_dir("fplite_order");
        let path = dir.join("playlist-ABC.fplite");
        // Non-ASCII path (mirrors the real profile) plus a bare file:// (skipped)
        // and a stream URL (kept verbatim).
        let bytes = fplite_bytes(
            true,
            &[
                "file://C:\\Users\\LENOVO\\Downloads\\Saint-Saëns Mélodie.mp3",
                "file://C:\\Music\\two.flac",
                "file://",
                "http://radio.example/stream",
            ],
        );
        std::fs::write(&path, &bytes).unwrap();

        let mut pl = mem();
        let summary = pl.import_fplite_report(&path).unwrap();
        assert_eq!(summary.imported, 3);
        assert_eq!(summary.skipped, 1, "bare file:// should be skipped");

        let got = paths(&pl, summary.playlist_id);
        assert_eq!(
            got,
            vec![
                "C:\\Users\\LENOVO\\Downloads\\Saint-Saëns Mélodie.mp3".to_string(),
                "C:\\Music\\two.flac".to_string(),
                "http://radio.example/stream".to_string(),
            ]
        );
        // Named after the file stem.
        assert_eq!(pl.list().unwrap()[0].title, "playlist-ABC");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fplite_sqlite_magic_is_reported_not_guessed() {
        let dir = tmp_dir("fplite_sqlite");
        let path = dir.join("playlist-SQL.fplite");
        std::fs::write(&path, b"SQLite format 3\0rest-of-header").unwrap();
        let mut pl = mem();
        let err = pl.import_fplite(&path).unwrap_err();
        assert!(matches!(err, LyraError::UnsupportedFormat(_)), "got {err:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fpl_binary_best_effort_scans_embedded_paths() {
        // Classic .fpl blob: header junk, two valid file:// paths separated by NUL
        // records, a file:// followed by invalid UTF-8 (skipped), then trailing junk.
        let mut blob: Vec<u8> = Vec::new();
        blob.extend_from_slice(&[0x01, 0x02, 0x00, 0x00]);
        blob.extend_from_slice(b"file://C:\\Music\\a.flac");
        blob.push(0x00);
        blob.extend_from_slice(b"file://C:\\Music\\b.flac");
        blob.push(0x00);
        blob.extend_from_slice(b"file://");
        blob.extend_from_slice(&[0xFF, 0xFE]); // invalid UTF-8 after the scheme
        blob.push(0x00);
        blob.extend_from_slice(&[0x10, 0x11]);

        let dir = tmp_dir("fpl_bin");
        let path = dir.join("legacy.fpl");
        std::fs::write(&path, &blob).unwrap();

        let mut pl = mem();
        let summary = pl.import_fpl_report(&path).unwrap();
        assert_eq!(summary.imported, 2);
        assert_eq!(summary.skipped, 1);
        assert_eq!(
            paths(&pl, summary.playlist_id),
            vec!["C:\\Music\\a.flac".to_string(), "C:\\Music\\b.flac".to_string()]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn import_foobar_playlist_dispatches_by_extension() {
        let mut pl = mem();
        let err = pl.import_foobar_playlist(Path::new("x.txt")).unwrap_err();
        assert!(matches!(err, LyraError::UnsupportedFormat(_)), "got {err:?}");
    }

    #[test]
    fn migrate_reads_index_order_and_reports_missing() {
        let profile = tmp_dir("fb2k_profile");
        let dir = profile.join("playlists-v2.0");
        std::fs::create_dir_all(&dir).unwrap();
        // Two playlists in index order; the second file is intentionally absent.
        std::fs::write(
            dir.join("index.txt"),
            "GUID-1:Default Playlist\nGUID-2:hoai-test\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("playlist-GUID-1.fplite"),
            fplite_bytes(true, &["file://C:\\one.flac", "file://C:\\two.flac"]),
        )
        .unwrap();

        let mut pl = mem();
        let report = pl.migrate_foobar_playlists(&profile).unwrap();
        assert_eq!(report.len(), 2);
        // foobar2000's scratch playlist is imported under a distinct title so it
        // never duplicates Lyra's own "Default Playlist".
        assert_eq!(report[0].0, IMPORTED_DEFAULT_TITLE);
        let s = report[0].1.as_ref().unwrap();
        assert_eq!(s.imported, 2);
        assert_eq!(pl.list().unwrap()[0].title, IMPORTED_DEFAULT_TITLE);
        // Second playlist's file is missing: reported as an error, not guessed.
        assert_eq!(report[1].0, "hoai-test");
        assert!(report[1].1.is_err());

        std::fs::remove_dir_all(&profile).ok();
    }

    #[test]
    fn index_parse_splits_on_first_colon() {
        let got = parse_index(b"\xEF\xBB\xBFABC-123:Title: With Colon\n\nDEF:Plain\n");
        assert_eq!(
            got,
            vec![
                ("ABC-123".to_string(), "Title: With Colon".to_string()),
                ("DEF".to_string(), "Plain".to_string()),
            ]
        );
    }

    #[test]
    fn m3u_parse_resolves_relative_and_skips_comments() {
        let m3u = b"#EXTM3U\n#EXTINF:123,Artist - Title\nsong.flac\n\nC:\\abs\\track.flac\n";
        let base = Path::new("C:\\music\\album");
        let items = parse_m3u(m3u, base.parent());
        // base.parent() of "C:\music\album" is "C:\music"
        assert_eq!(items.len(), 2);
        assert!(items[0].path.ends_with("song.flac"));
        assert!(items[0].path.contains("music"));
        assert_eq!(items[1].path, "C:\\abs\\track.flac");
    }

    #[test]
    fn m3u_round_trip_preserves_order() {
        let dir = std::env::temp_dir().join(format!("lyra_m3u_rt_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let m3u_path = dir.join("list.m3u8");

        // Use absolute paths so resolution is a no-op and the round trip is exact.
        let p1 = dir.join("one.flac");
        let p2 = dir.join("two.flac");
        let originals = vec![
            TrackRef::file(p1.to_string_lossy().into_owned()),
            TrackRef::file(p2.to_string_lossy().into_owned()),
        ];

        let mut src = mem();
        let sid = src.create("src", false).unwrap();
        src.add_items(sid, &originals).unwrap();
        src.export_m3u(sid, &m3u_path).unwrap();

        // Import into a fresh database.
        let mut dst = mem();
        let did = dst.import_m3u(&m3u_path).unwrap();
        let imported = dst.items(did).unwrap();

        let want: Vec<&str> = originals.iter().map(|t| t.path.as_str()).collect();
        let got: Vec<&str> = imported.iter().map(|t| t.path.as_str()).collect();
        assert_eq!(got, want);
        // The imported playlist is named after the file stem.
        assert_eq!(dst.list().unwrap()[0].title, "list");

        std::fs::remove_dir_all(&dir).ok();
    }

    fn paths(pl: &Playlists, id: i64) -> Vec<String> {
        pl.items(id).unwrap().into_iter().map(|t| t.path).collect()
    }

    /// Opt-in: migrate the real foobar2000-v2 profile (read-only).
    ///
    /// Ignored by default because the profile may be absent or locked in CI. Run
    /// with `cargo test -p lyra-library -- --ignored --nocapture`; set
    /// `LYRA_FB2K_PROFILE` to override the default `%APPDATA%\foobar2000-v2` path.
    /// Prints each playlist's title, imported/skipped counts and first items so QA
    /// can compare them with Beefweb.
    #[test]
    #[ignore = "reads the real foobar2000 profile; opt-in"]
    fn migrate_real_profile_opt_in() {
        let profile = std::env::var_os("LYRA_FB2K_PROFILE")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("APPDATA").map(|a| Path::new(&a).join("foobar2000-v2"))
            })
            .expect("no profile path");
        eprintln!("migrating {}", profile.display());
        let mut pl = mem();
        let report = pl.migrate_foobar_playlists(&profile).expect("migrate");
        for (title, result) in &report {
            match result {
                Ok(s) => {
                    eprintln!("OK   {title:?}: {} imported, {} skipped", s.imported, s.skipped);
                    for item in pl.items(s.playlist_id).unwrap().iter().take(3) {
                        eprintln!("       - {}", item.path);
                    }
                }
                Err(e) => eprintln!("FAIL {title:?}: {e}"),
            }
        }
    }
}
