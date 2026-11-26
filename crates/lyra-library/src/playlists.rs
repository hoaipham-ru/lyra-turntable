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
//! directory). foobar2000's `.fpl` is undocumented, so [`Playlists::import_fpl`] is
//! a best-effort stub that reports an [`LyraError::UnsupportedFormat`] rather than
//! guessing paths; full `.fpl` support is deferred to P6.

use std::path::Path;

use rusqlite::{params, Connection};

use lyra_core::{LyraError, Result, TrackRef};

/// The reserved title of the Default Playlist.
pub const DEFAULT_PLAYLIST_TITLE: &str = "Default Playlist";

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

    /// Best-effort `.fpl` import. foobar2000's `.fpl` format is undocumented, so
    /// rather than guess paths this reports the format as unsupported for now;
    /// real parsing is deferred to P6.
    ///
    /// # Errors
    /// Always returns [`LyraError::UnsupportedFormat`].
    pub fn import_fpl(&mut self, path: &Path) -> Result<i64> {
        Err(LyraError::UnsupportedFormat(format!(
            ".fpl import is not supported yet (undocumented format): {}",
            path.display()
        )))
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

/// Wrap a `rusqlite` error as a [`LyraError::Io`] with a context message.
fn db_err(context: &str, e: &rusqlite::Error) -> LyraError {
    LyraError::Io(format!("playlist store ({context}): {e}"))
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

    #[test]
    fn fpl_is_unsupported() {
        let mut pl = mem();
        let err = pl.import_fpl(Path::new("x.fpl")).unwrap_err();
        assert!(matches!(err, LyraError::UnsupportedFormat(_)), "got {err:?}");
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
}
