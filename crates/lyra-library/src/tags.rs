//! Read-only metadata reading (S06, P4.2).
//!
//! [`read_tags`] opens an audio file with `lofty`, reading only its tags and
//! format headers — it **never decodes audio** and **never writes**. The result,
//! a [`TrackMeta`], carries both a set of typed convenience fields (title, artist,
//! album, …) mapped across tag formats and the raw [`TagMap`] of every tag the
//! file carried, keyed by the source container's own field names (e.g. Vorbis
//! comment `TITLE`/`ARTIST`/`ALBUM`) so later parity work can compare apples to
//! apples with foobar2000's Beefweb columns.
//!
//! # Scope note
//! This is the P4.2 functional reader the app alpha needs to show track rows. Full
//! field-by-field parity with foobar2000's title formatting is proven in P6; a few
//! non-standard tags that `lofty` does not map to a known key are dropped from the
//! raw map for now (documented in the handoff), and [`TrackMeta::duration_frames`]
//! is derived from the reported duration and sample rate, so it is approximate —
//! the exact frame count comes from the decoder probe.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::read_from_path;
use lofty::tag::{Accessor, ItemKey, Tag};

use lyra_core::{LyraError, Result, TagMap};

/// Metadata read from a single audio file.
///
/// Typed fields are normalised across tag formats via `lofty`; [`TrackMeta::tags`]
/// keeps the raw per-container field names. All metadata fields are optional
/// because not every file carries them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackMeta {
    /// The file this metadata was read from.
    pub path: PathBuf,
    /// Track title (`%title%`).
    pub title: Option<String>,
    /// Track artist (`%artist%`).
    pub artist: Option<String>,
    /// Album title (`%album%`).
    pub album: Option<String>,
    /// Album artist (`%album artist%`).
    pub album_artist: Option<String>,
    /// Track number as written (`%tracknumber%`); kept as a string because sources
    /// may write `3`, `03` or `3/12`.
    pub track_number: Option<String>,
    /// Release date or year (`%date%`), as written.
    pub date: Option<String>,
    /// Genre (`%genre%`).
    pub genre: Option<String>,
    /// Playback duration, if the format reported one.
    pub duration: Option<Duration>,
    /// Approximate length in source sample frames, derived from
    /// [`duration`](Self::duration) × [`sample_rate`](Self::sample_rate). `None`
    /// unless both are known. Not frame-exact — the decoder probe gives the exact
    /// count.
    pub duration_frames: Option<u64>,
    /// Sample rate in Hz, if reported.
    pub sample_rate: Option<u32>,
    /// Channel count, if reported.
    pub channels: Option<u16>,
    /// Bits per sample, if reported.
    pub bits: Option<u8>,
    /// Every tag the file carried, keyed by the source container's own field names.
    pub tags: TagMap,
}

/// Read tags and format info from the file at `path`.
///
/// Opens the file read-only and parses only its tags and headers; it never decodes
/// audio and never writes. A file `lofty` cannot parse returns an error rather than
/// panicking.
///
/// # Errors
/// Returns [`LyraError::Io`] if the file cannot be opened and
/// [`LyraError::UnsupportedFormat`] if `lofty` cannot parse it.
pub fn read_tags(path: &Path) -> Result<TrackMeta> {
    let tagged = read_from_path(path).map_err(|e| map_lofty_err(path, &e))?;

    let props = tagged.properties();
    let duration = {
        let d = props.duration();
        if d.is_zero() {
            None
        } else {
            Some(d)
        }
    };
    let sample_rate = props.sample_rate();
    let channels = props.channels().map(u16::from);
    let bits = props.bit_depth();
    let duration_frames = match (duration, sample_rate) {
        (Some(d), Some(rate)) if rate > 0 => Some((d.as_secs_f64() * f64::from(rate)).round() as u64),
        _ => None,
    };

    // Prefer the primary tag (the format's native tag); fall back to the first tag.
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let (title, artist, album, album_artist, track_number, date, genre, tags) = match tag {
        Some(t) => (
            t.title().map(cow_to_string),
            t.artist().map(cow_to_string),
            t.album().map(cow_to_string),
            t.get_string(ItemKey::AlbumArtist).map(str::to_string),
            track_number_of(t),
            date_of(t),
            t.genre().map(cow_to_string),
            raw_tag_map(t),
        ),
        None => (None, None, None, None, None, None, None, TagMap::new()),
    };

    Ok(TrackMeta {
        path: path.to_path_buf(),
        title,
        artist,
        album,
        album_artist,
        track_number,
        date,
        genre,
        duration,
        duration_frames,
        sample_rate,
        channels,
        bits,
        tags,
    })
}

/// Map a `lofty` read error to a [`LyraError`].
///
/// An underlying [`std::io::Error`] anywhere in the source chain (e.g. a missing
/// file) becomes [`LyraError::Io`]; anything else (an unparseable or unknown
/// container) becomes [`LyraError::UnsupportedFormat`].
fn map_lofty_err(path: &Path, e: &lofty::error::FileParseError) -> LyraError {
    use std::error::Error;
    let msg = format!("reading tags from {}: {e}", path.display());
    let mut src: Option<&(dyn Error + 'static)> = Some(e);
    while let Some(s) = src {
        if s.is::<std::io::Error>() {
            return LyraError::Io(msg);
        }
        src = s.source();
    }
    LyraError::UnsupportedFormat(msg)
}

/// The track number as written, trying the raw tag string first (so `3/12`
/// survives) and falling back to the numeric accessor.
fn track_number_of(tag: &Tag) -> Option<String> {
    tag.get_string(ItemKey::TrackNumber)
        .map(str::to_string)
        .or_else(|| tag.track().map(|n| n.to_string()))
}

/// The release date/year as written: the recording date string if present, else
/// the year.
fn date_of(tag: &Tag) -> Option<String> {
    tag.get_string(ItemKey::RecordingDate)
        .map(str::to_string)
        .or_else(|| tag.get_string(ItemKey::Year).map(str::to_string))
}

/// Build the raw [`TagMap`] from a tag, keyed by the container's native field
/// names. Items whose key `lofty` cannot map back to a native name are skipped
/// (documented limitation for P4.2). Multiple values for one key are joined with
/// `"; "` so a single-valued map still shows every value.
fn raw_tag_map(tag: &Tag) -> TagMap {
    let tag_type = tag.tag_type();
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for item in tag.items() {
        let Some(key) = item.key().map_key(tag_type) else {
            continue;
        };
        let Some(value) = item.value().text() else {
            continue;
        };
        grouped
            .entry(key.to_string())
            .or_default()
            .push(value.to_string());
    }
    let mut map = TagMap::new();
    for (key, values) in grouped {
        map.insert(key, values.join("; "));
    }
    map
}

/// Convert a borrowed `Cow<str>` accessor result into an owned `String`.
fn cow_to_string(c: std::borrow::Cow<'_, str>) -> String {
    c.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_io_error() {
        let err = read_tags(Path::new("C:/no/such/file.flac")).unwrap_err();
        assert!(matches!(err, LyraError::Io(_)), "got {err:?}");
    }

    #[test]
    fn non_audio_file_is_unsupported() {
        let dir = std::env::temp_dir().join(format!("lyra_tags_na_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("not_audio.flac");
        std::fs::write(&p, b"this is not a flac file at all").unwrap();
        let err = read_tags(&p).unwrap_err();
        assert!(matches!(err, LyraError::UnsupportedFormat(_)), "got {err:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
