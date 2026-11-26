//! SQLite-backed tag cache, optional folder scan and optional folder watcher
//! (S06, P6.4).
//!
//! [`TagCache`] memoises [`crate::read_tags`] by `(path, mtime, size)`: a repeated
//! read of an unchanged file is served from SQLite instead of re-opening the audio
//! container. The cache is **read-only with respect to the audio library** — it
//! stats files and reads their tags, but never writes, renames or retags an audio
//! file. Only its own database is written.
//!
//! # Schema
//! ```text
//! tag_cache(path PRIMARY KEY, mtime, size, <typed TrackMeta columns…>, tags_json)
//! ```
//! The key is the file's absolute path. `mtime` is stored as whole Unix seconds and
//! `size` as the byte length; a row whose `(mtime, size)` no longer matches the file
//! on disk is treated as stale and overwritten on the next read.
//!
//! # No contract leak
//! Neither [`TrackMeta`] nor [`lyra_core::TagMap`] is `Serialize`, and this module
//! does **not** add serde derives to them (that would be a `lyra-core` contract
//! change). Instead every typed [`TrackMeta`] field is stored in its own explicit
//! column, and the raw [`lyra_core::TagMap`] is serialised here, by hand, into a
//! flat JSON object of `{ "KEY": "value" }` strings via `serde_json`. The mapping
//! lives entirely in this crate.
//!
//! # Optional pieces
//! * [`TagCache::scan_folder`] walks a directory and caches every audio-like file —
//!   explicitly optional, best-effort, and never panics on an unreadable file.
//! * The `watch` Cargo feature adds [`watcher::Watcher`], a thin wrapper over the
//!   `notify` crate that reports changed paths so the cache can be invalidated. It
//!   is off by default, so the standard build pulls no `notify` dependency and
//!   spawns no watcher thread.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use lyra_core::{LyraError, Result, TagMap};

use crate::tags::{self, TrackMeta};

/// A SQLite-backed, `(path, mtime, size)`-keyed cache over [`crate::read_tags`].
pub struct TagCache {
    conn: Connection,
}

impl TagCache {
    /// Open (creating if necessary) the tag-cache database at `path`.
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
        let cache = Self { conn };
        cache.init_schema()?;
        Ok(cache)
    }

    /// Create the schema if it does not yet exist.
    fn init_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS tag_cache (
                     path                  TEXT    PRIMARY KEY,
                     mtime                 INTEGER NOT NULL,
                     size                  INTEGER NOT NULL,
                     title                 TEXT,
                     artist                TEXT,
                     album                 TEXT,
                     album_artist          TEXT,
                     track_number          TEXT,
                     disc_number           TEXT,
                     date                  TEXT,
                     genre                 TEXT,
                     replaygain_track_gain TEXT,
                     replaygain_album_gain TEXT,
                     replaygain_track_peak TEXT,
                     replaygain_album_peak TEXT,
                     codec                 TEXT,
                     duration_nanos        INTEGER,
                     duration_frames       INTEGER,
                     sample_rate           INTEGER,
                     channels              INTEGER,
                     bits                  INTEGER,
                     tags_json             TEXT    NOT NULL
                 );",
            )
            .map_err(|e| db_err("create schema", &e))
    }

    /// Return the metadata for `path`, from cache when fresh, else by reading.
    ///
    /// Stats the file for its modification time and size. If a cached row exists
    /// whose `(mtime, size)` match the file on disk, the cached [`TrackMeta`] is
    /// returned without touching the audio file. Otherwise [`crate::read_tags`] is
    /// called, the (possibly stale) row is overwritten with the fresh result, and
    /// the fresh metadata is returned.
    ///
    /// Reading never modifies the audio file.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] if the file cannot be stat-ed or a database error
    /// occurs, or [`LyraError::UnsupportedFormat`] if the file cannot be parsed.
    pub fn get_or_read(&mut self, path: &Path) -> Result<TrackMeta> {
        let key = abs_key(path);
        let key_str = key.to_string_lossy();
        let (mtime, size) = stat(path)?;

        if let Some(meta) = self.lookup_fresh(&key_str, mtime, size)? {
            return Ok(meta);
        }

        let meta = tags::read_tags(path)?;
        self.store(&key_str, mtime, size, &meta)?;
        Ok(meta)
    }

    /// Fetch a cached row only if it is fresh for `(mtime, size)`.
    fn lookup_fresh(&self, key: &str, mtime: i64, size: i64) -> Result<Option<TrackMeta>> {
        self.conn
            .query_row(
                "SELECT title, artist, album, album_artist, track_number, disc_number,
                        date, genre, replaygain_track_gain, replaygain_album_gain,
                        replaygain_track_peak, replaygain_album_peak, codec,
                        duration_nanos, duration_frames, sample_rate, channels, bits,
                        tags_json
                 FROM tag_cache
                 WHERE path = ?1 AND mtime = ?2 AND size = ?3",
                params![key, mtime, size],
                |row| Ok(row_to_meta(key, row)),
            )
            .optional()
            .map_err(|e| db_err("cache lookup", &e))?
            .transpose()
    }

    /// Insert or replace the cached row for `key`.
    fn store(&mut self, key: &str, mtime: i64, size: i64, meta: &TrackMeta) -> Result<()> {
        let tags_json = tags_to_json(&meta.tags);
        self.conn
            .execute(
                "INSERT OR REPLACE INTO tag_cache (
                     path, mtime, size, title, artist, album, album_artist,
                     track_number, disc_number, date, genre,
                     replaygain_track_gain, replaygain_album_gain,
                     replaygain_track_peak, replaygain_album_peak, codec,
                     duration_nanos, duration_frames, sample_rate, channels, bits,
                     tags_json
                 ) VALUES (
                     ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22
                 )",
                params![
                    key,
                    mtime,
                    size,
                    meta.title,
                    meta.artist,
                    meta.album,
                    meta.album_artist,
                    meta.track_number,
                    meta.disc_number,
                    meta.date,
                    meta.genre,
                    meta.replaygain_track_gain,
                    meta.replaygain_album_gain,
                    meta.replaygain_track_peak,
                    meta.replaygain_album_peak,
                    meta.codec,
                    meta.duration.map(|d| d.as_nanos() as i64),
                    meta.duration_frames.map(|f| f as i64),
                    meta.sample_rate.map(i64::from),
                    meta.channels.map(i64::from),
                    meta.bits.map(i64::from),
                    tags_json,
                ],
            )
            .map_err(|e| db_err("cache store", &e))?;
        Ok(())
    }

    /// Drop the cached row for `path`, if any.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn invalidate(&mut self, path: &Path) -> Result<()> {
        let key = abs_key(path);
        self.conn
            .execute(
                "DELETE FROM tag_cache WHERE path = ?1",
                params![key.to_string_lossy()],
            )
            .map_err(|e| db_err("invalidate", &e))?;
        Ok(())
    }

    /// Empty the entire cache.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn clear(&mut self) -> Result<()> {
        self.conn
            .execute("DELETE FROM tag_cache", [])
            .map_err(|e| db_err("clear cache", &e))?;
        Ok(())
    }

    /// Number of rows currently cached.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn len(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tag_cache", [], |r| r.get(0))
            .map_err(|e| db_err("count", &e))?;
        Ok(n as usize)
    }

    /// `true` if the cache holds no rows.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] on a database error.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Walk `root` and cache every audio-like file found, returning the paths that
    /// were successfully read into the cache (optional, best-effort — P6.4).
    ///
    /// When `recursive` is true, sub-directories are visited depth-first. Files
    /// whose extension is not audio-like are ignored. A file that cannot be stat-ed
    /// or parsed is skipped (logged to stderr) rather than aborting the scan, so a
    /// single broken file never fails the whole walk and the function never panics.
    ///
    /// The returned list contains only files that read successfully; a skipped,
    /// unreadable file is absent from it.
    ///
    /// Read-only: this never writes, renames or retags any file under `root`.
    ///
    /// # Errors
    /// Returns [`LyraError::Io`] only if `root` itself cannot be read as a
    /// directory; per-file errors are swallowed and skipped.
    pub fn scan_folder(&mut self, root: &Path, recursive: bool) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        self.scan_into(root, recursive, &mut out)?;
        Ok(out)
    }

    /// Recursive worker for [`TagCache::scan_folder`].
    fn scan_into(&mut self, dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> Result<()> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| LyraError::Io(format!("scanning folder {}: {e}", dir.display())))?;
        for entry in entries {
            let Ok(entry) = entry else {
                continue; // skip entries we cannot even read the name of
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                if recursive {
                    // A sub-directory we cannot open is skipped, not fatal.
                    let _ = self.scan_into(&path, recursive, out);
                }
                continue;
            }
            if !is_audio_like(&path) {
                continue;
            }
            match self.get_or_read(&path) {
                Ok(_) => out.push(path),
                Err(e) => {
                    eprintln!(
                        "lyra-library: skipping {} during scan: {e}",
                        path.display()
                    );
                }
            }
        }
        Ok(())
    }
}

/// Audio-like file extensions the scan considers. Matched case-insensitively.
///
/// This is the *scan filter* — a cheap gate so the walk does not try to read every
/// cover image and `.cue` as audio. Whether a file truly parses is still decided by
/// [`crate::read_tags`]; a `.flac` with garbage inside is simply skipped.
const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "wav", "wave", "mp3", "m4a", "mp4", "aac", "ogg", "oga", "opus", "aif",
    "aiff", "ape", "wv", "mpc", "dsf", "dff",
];

/// Whether `path` has an audio-like extension (case-insensitive).
fn is_audio_like(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_ascii_lowercase();
            AUDIO_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

/// Normalise `path` to an absolute path for use as the cache key, without
/// requiring the file to exist or resolving symlinks (so the stored key is stable
/// and matches what callers pass). Falls back to the input on error.
fn abs_key(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Stat a file for its modification time (whole Unix seconds) and byte size.
///
/// # Errors
/// Returns [`LyraError::Io`] if the file cannot be stat-ed.
fn stat(path: &Path) -> Result<(i64, i64)> {
    let md = std::fs::metadata(path)
        .map_err(|e| LyraError::Io(format!("stat {}: {e}", path.display())))?;
    let size = md.len() as i64;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok((mtime, size))
}

/// Reconstruct a [`TrackMeta`] from a cached row.
///
/// Column order matches the `SELECT` in [`TagCache::lookup_fresh`]. `path` is the
/// cache key (an absolute path string).
fn row_to_meta(path: &str, row: &rusqlite::Row<'_>) -> Result<TrackMeta> {
    let duration_nanos: Option<i64> = row.get(13).map_err(col_err)?;
    let duration_frames: Option<i64> = row.get(14).map_err(col_err)?;
    let sample_rate: Option<i64> = row.get(15).map_err(col_err)?;
    let channels: Option<i64> = row.get(16).map_err(col_err)?;
    let bits: Option<i64> = row.get(17).map_err(col_err)?;
    let tags_json: String = row.get(18).map_err(col_err)?;

    Ok(TrackMeta {
        path: PathBuf::from(path),
        title: row.get(0).map_err(col_err)?,
        artist: row.get(1).map_err(col_err)?,
        album: row.get(2).map_err(col_err)?,
        album_artist: row.get(3).map_err(col_err)?,
        track_number: row.get(4).map_err(col_err)?,
        disc_number: row.get(5).map_err(col_err)?,
        date: row.get(6).map_err(col_err)?,
        genre: row.get(7).map_err(col_err)?,
        replaygain_track_gain: row.get(8).map_err(col_err)?,
        replaygain_album_gain: row.get(9).map_err(col_err)?,
        replaygain_track_peak: row.get(10).map_err(col_err)?,
        replaygain_album_peak: row.get(11).map_err(col_err)?,
        codec: row.get(12).map_err(col_err)?,
        duration: duration_nanos.map(|n| Duration::from_nanos(n as u64)),
        duration_frames: duration_frames.map(|f| f as u64),
        sample_rate: sample_rate.map(|s| s as u32),
        channels: channels.map(|c| c as u16),
        bits: bits.map(|b| b as u8),
        tags: tags_from_json(&tags_json),
    })
}

/// Serialise a [`TagMap`] into a flat JSON object string `{ "KEY": "value" }`.
///
/// Done by hand here (not via a `Serialize` impl on `lyra-core`'s `TagMap`) so no
/// contract change is forced on `lyra-core`.
fn tags_to_json(tags: &TagMap) -> String {
    let obj: serde_json::Map<String, serde_json::Value> = tags
        .iter()
        .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
        .collect();
    serde_json::Value::Object(obj).to_string()
}

/// Rebuild a [`TagMap`] from the JSON produced by [`tags_to_json`].
///
/// Tolerant by design: malformed JSON or a non-object yields an empty map, and
/// non-string values are skipped, so a corrupt cache row degrades to "no raw tags"
/// rather than erroring a read.
fn tags_from_json(s: &str) -> TagMap {
    let mut map = TagMap::new();
    if let Ok(serde_json::Value::Object(obj)) = serde_json::from_str::<serde_json::Value>(s) {
        for (k, v) in obj {
            if let Some(val) = v.as_str() {
                map.insert(k, val.to_string());
            }
        }
    }
    map
}

/// Wrap a `rusqlite` column-decode error as a [`LyraError::Io`].
fn col_err(e: rusqlite::Error) -> LyraError {
    db_err("decode cached row", &e)
}

/// Wrap a `rusqlite` error as a [`LyraError::Io`] with a context message.
fn db_err(context: &str, e: &rusqlite::Error) -> LyraError {
    LyraError::Io(format!("tag cache ({context}): {e}"))
}

#[cfg(feature = "watch")]
pub mod watcher {
    //! Optional folder watcher over the `notify` crate (`watch` feature).
    //!
    //! [`Watcher`] wraps a `notify` recommended watcher and forwards the changed
    //! paths of every filesystem event to a user callback, so a caller can
    //! [`TagCache::invalidate`](super::TagCache::invalidate) the affected entries.
    //! The watcher thread is owned by `notify` and lives as long as the [`Watcher`]
    //! value; dropping it stops watching. Constructing a [`Watcher`] is the only way
    //! this crate ever starts a background thread, and it only happens behind this
    //! off-by-default feature.

    use std::path::{Path, PathBuf};

    use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

    use lyra_core::{LyraError, Result};

    /// A live folder watcher. Keep it alive to keep receiving events; drop it to
    /// stop.
    pub struct Watcher {
        _inner: RecommendedWatcher,
    }

    impl Watcher {
        /// Start watching `root` (recursively when `recursive` is true), calling
        /// `on_change` with the changed paths of each filesystem event.
        ///
        /// The callback runs on `notify`'s watcher thread, so it must be `Send`
        /// and `'static` and should be cheap (e.g. push paths onto a channel the
        /// caller drains to invalidate the cache).
        ///
        /// # Errors
        /// Returns [`LyraError::Io`] if the platform watcher cannot be created or
        /// `root` cannot be watched.
        pub fn new<F>(root: &Path, recursive: bool, on_change: F) -> Result<Self>
        where
            F: Fn(Vec<PathBuf>) + Send + 'static,
        {
            let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    on_change(event.paths);
                }
            })
            .map_err(|e| LyraError::Io(format!("creating folder watcher: {e}")))?;

            let mode = if recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            inner
                .watch(root, mode)
                .map_err(|e| LyraError::Io(format!("watching {}: {e}", root.display())))?;

            Ok(Self { _inner: inner })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal, valid 16-bit PCM WAV file (mono, 44.1 kHz) with `frames`
    /// silent sample frames, so `lofty` parses it as a real audio file.
    fn wav_bytes(frames: u32) -> Vec<u8> {
        let channels: u16 = 1;
        let sample_rate: u32 = 44_100;
        let bits: u16 = 16;
        let block_align: u16 = channels * bits / 8;
        let byte_rate: u32 = sample_rate * u32::from(block_align);
        let data_len: u32 = frames * u32::from(block_align);

        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&sample_rate.to_le_bytes());
        b.extend_from_slice(&byte_rate.to_le_bytes());
        b.extend_from_slice(&block_align.to_le_bytes());
        b.extend_from_slice(&bits.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        b.resize(b.len() + data_len as usize, 0);
        b
    }

    /// A unique temp directory for one test, created fresh.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lyra_cache_{tag}_{}_{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mem() -> TagCache {
        TagCache::open(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn memory_db_opens_and_is_empty() {
        let cache = mem();
        assert!(cache.is_empty().unwrap());
    }

    #[test]
    fn cache_hit_serves_stored_row_not_the_file() {
        let dir = temp_dir("hit");
        let p = dir.join("a.wav");
        std::fs::write(&p, wav_bytes(1000)).unwrap();

        let mut cache = mem();
        let first = cache.get_or_read(&p).unwrap();
        assert_eq!(first.codec.as_deref(), Some("PCM"));
        assert_eq!(cache.len().unwrap(), 1);

        // Poison the stored row: a genuine re-read of the file could never produce
        // this title, so seeing it back proves the second call hit the cache.
        let key = abs_key(&p);
        cache
            .conn
            .execute(
                "UPDATE tag_cache SET title = 'SENTINEL' WHERE path = ?1",
                params![key.to_string_lossy()],
            )
            .unwrap();

        let second = cache.get_or_read(&p).unwrap();
        assert_eq!(second.title.as_deref(), Some("SENTINEL"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn size_change_forces_reread() {
        let dir = temp_dir("size");
        let p = dir.join("a.wav");
        std::fs::write(&p, wav_bytes(1000)).unwrap();

        let mut cache = mem();
        cache.get_or_read(&p).unwrap();

        // Poison, then grow the file so its size no longer matches the cached row.
        let key = abs_key(&p);
        cache
            .conn
            .execute(
                "UPDATE tag_cache SET title = 'STALE' WHERE path = ?1",
                params![key.to_string_lossy()],
            )
            .unwrap();
        std::fs::write(&p, wav_bytes(5000)).unwrap();

        let fresh = cache.get_or_read(&p).unwrap();
        // The size mismatch forced a real re-read, overwriting the poisoned title.
        assert_eq!(fresh.title, None);
        assert_eq!(cache.len().unwrap(), 1, "stale row overwritten, not duplicated");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mtime_change_forces_reread() {
        let dir = temp_dir("mtime");
        let p = dir.join("a.wav");
        std::fs::write(&p, wav_bytes(1000)).unwrap();

        let mut cache = mem();
        cache.get_or_read(&p).unwrap();

        // Simulate the file's mtime having advanced since caching: poison the row
        // and push its stored mtime into the past so it no longer matches on disk.
        let key = abs_key(&p);
        cache
            .conn
            .execute(
                "UPDATE tag_cache SET title = 'STALE', mtime = mtime - 10000 WHERE path = ?1",
                params![key.to_string_lossy()],
            )
            .unwrap();

        let fresh = cache.get_or_read(&p).unwrap();
        assert_eq!(fresh.title, None, "mtime mismatch forces a re-read");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn round_trips_typed_and_raw_fields() {
        let dir = temp_dir("rt");
        let p = dir.join("a.wav");
        std::fs::write(&p, wav_bytes(44_100)).unwrap();

        let mut cache = mem();
        let direct = tags::read_tags(&p).unwrap();
        let cached = cache.get_or_read(&p).unwrap();
        // First call reads the file; the stored row must reconstruct identically.
        let from_cache = cache.get_or_read(&p).unwrap();
        assert_eq!(direct, cached);
        assert_eq!(direct, from_cache);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalidate_and_clear() {
        let dir = temp_dir("inv");
        let p = dir.join("a.wav");
        std::fs::write(&p, wav_bytes(1000)).unwrap();

        let mut cache = mem();
        cache.get_or_read(&p).unwrap();
        assert_eq!(cache.len().unwrap(), 1);

        cache.invalidate(&p).unwrap();
        assert_eq!(cache.len().unwrap(), 0);

        cache.get_or_read(&p).unwrap();
        cache.clear().unwrap();
        assert!(cache.is_empty().unwrap());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tag_json_round_trip() {
        let mut tags = TagMap::new();
        tags.insert("TITLE", "Song");
        tags.insert("ARTIST", "Alice; Bob");
        let json = tags_to_json(&tags);
        let back = tags_from_json(&json);
        assert_eq!(back.get("TITLE"), Some("Song"));
        assert_eq!(back.get("ARTIST"), Some("Alice; Bob"));
        // Corrupt input degrades to an empty map rather than panicking.
        assert!(tags_from_json("not json").is_empty());
    }

    #[test]
    fn scan_folder_skips_unreadable_and_non_audio() {
        let dir = temp_dir("scan");
        std::fs::write(dir.join("one.wav"), wav_bytes(1000)).unwrap();
        std::fs::write(dir.join("two.wav"), wav_bytes(2000)).unwrap();
        // An audio-like extension but garbage contents: must be skipped, not fatal.
        std::fs::write(dir.join("broken.flac"), b"not a flac").unwrap();
        // A non-audio file: ignored by the extension filter.
        std::fs::write(dir.join("notes.txt"), b"hello").unwrap();
        // A nested dir with another audio file, to exercise recursion.
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("three.wav"), wav_bytes(3000)).unwrap();

        let mut cache = mem();

        let flat = cache.scan_folder(&dir, false).unwrap();
        assert_eq!(flat.len(), 2, "two readable wavs at the top level");

        cache.clear().unwrap();
        let deep = cache.scan_folder(&dir, true).unwrap();
        assert_eq!(deep.len(), 3, "recursion also finds the nested wav");
        assert_eq!(cache.len().unwrap(), 3);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_missing_root_errors() {
        let mut cache = mem();
        let err = cache
            .scan_folder(Path::new("C:/no/such/dir/at/all"), false)
            .unwrap_err();
        assert!(matches!(err, LyraError::Io(_)), "got {err:?}");
    }

    #[cfg(feature = "watch")]
    #[test]
    fn watcher_constructs_on_a_real_dir() {
        let dir = temp_dir("watch");
        let w = watcher::Watcher::new(&dir, false, |_paths| {});
        assert!(w.is_ok(), "constructing the watcher should succeed");
        drop(w);
        std::fs::remove_dir_all(&dir).ok();
    }
}
