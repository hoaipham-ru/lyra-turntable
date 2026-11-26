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

use lofty::file::{AudioFile, FileType, TaggedFileExt};
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
    /// Disc number as written (`%discnumber%`); kept as a string because sources
    /// may write `1` or `1/2`.
    pub disc_number: Option<String>,
    /// Release date or year (`%date%`), as written.
    pub date: Option<String>,
    /// Genre (`%genre%`).
    pub genre: Option<String>,
    /// ReplayGain track gain (`%replaygain_track_gain%`), as written (e.g.
    /// `-6.48 dB`). From Vorbis comment `REPLAYGAIN_TRACK_GAIN` / ID3 `TXXX`.
    pub replaygain_track_gain: Option<String>,
    /// ReplayGain album gain (`%replaygain_album_gain%`), as written.
    pub replaygain_album_gain: Option<String>,
    /// ReplayGain track peak, as written (nice-to-have; not a Beefweb parity column).
    pub replaygain_track_peak: Option<String>,
    /// ReplayGain album peak, as written (nice-to-have; not a Beefweb parity column).
    pub replaygain_album_peak: Option<String>,
    /// Canonical codec name (`%codec%`), derived from the container's
    /// [`FileType`]. foobar2000's exact spelling/case may differ; that is an
    /// allowed parity difference (TEST-STRATEGY §6, "codec naming").
    pub codec: Option<String>,
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

    let codec = codec_from_filetype(tagged.file_type());

    // Prefer the primary tag (the format's native tag); fall back to the first tag.
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let fields = tag.map(TagFields::from_tag).unwrap_or_default();

    Ok(TrackMeta {
        path: path.to_path_buf(),
        title: fields.title,
        artist: fields.artist,
        album: fields.album,
        album_artist: fields.album_artist,
        track_number: fields.track_number,
        disc_number: fields.disc_number,
        date: fields.date,
        genre: fields.genre,
        replaygain_track_gain: fields.replaygain_track_gain,
        replaygain_album_gain: fields.replaygain_album_gain,
        replaygain_track_peak: fields.replaygain_track_peak,
        replaygain_album_peak: fields.replaygain_album_peak,
        codec,
        duration,
        duration_frames,
        sample_rate,
        channels,
        bits,
        tags: fields.tags,
    })
}

/// The tag-sourced fields of a [`TrackMeta`], gathered from a single `lofty`
/// [`Tag`]. Kept as a struct (rather than a wide tuple) so the extraction stays
/// readable as parity fields grow.
#[derive(Default)]
struct TagFields {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    album_artist: Option<String>,
    track_number: Option<String>,
    disc_number: Option<String>,
    date: Option<String>,
    genre: Option<String>,
    replaygain_track_gain: Option<String>,
    replaygain_album_gain: Option<String>,
    replaygain_track_peak: Option<String>,
    replaygain_album_peak: Option<String>,
    tags: TagMap,
}

impl TagFields {
    /// Extract the parity fields from a single `lofty` [`Tag`].
    ///
    /// Typed convenience fields take the tag's primary (first) value; every value
    /// of every multi-valued key is still retrievable from [`TagFields::tags`],
    /// which joins values with the chosen separator (`"; "`).
    fn from_tag(t: &Tag) -> Self {
        Self {
            title: t.title().map(cow_to_string),
            artist: t.artist().map(cow_to_string),
            album: t.album().map(cow_to_string),
            album_artist: t.get_string(ItemKey::AlbumArtist).map(str::to_string),
            track_number: track_number_of(t),
            disc_number: t.get_string(ItemKey::DiscNumber).map(str::to_string),
            date: date_of(t),
            genre: t.genre().map(cow_to_string),
            replaygain_track_gain: t
                .get_string(ItemKey::ReplayGainTrackGain)
                .map(str::to_string),
            replaygain_album_gain: t
                .get_string(ItemKey::ReplayGainAlbumGain)
                .map(str::to_string),
            replaygain_track_peak: t
                .get_string(ItemKey::ReplayGainTrackPeak)
                .map(str::to_string),
            replaygain_album_peak: t
                .get_string(ItemKey::ReplayGainAlbumPeak)
                .map(str::to_string),
            tags: raw_tag_map(t),
        }
    }
}

/// Map a `lofty` [`FileType`] to a canonical `%codec%` string.
///
/// The spelling and case here are Lyra's own canonical choice; foobar2000 may
/// print the codec differently (e.g. `flac` vs `FLAC`, or `PCM` vs `WAV`), which
/// TEST-STRATEGY §6 lists as an allowed parity difference ("codec naming").
///
/// Notes on ambiguous containers:
/// * `Mpeg` covers MP1/MP2/MP3; the corpus is MP3, so it maps to `MP3`.
/// * `Mp4` can hold AAC **or** ALAC; `FileType` alone cannot tell them apart, so
///   it maps to `MP4` (disambiguation would need the decoder probe).
/// * DSD (`.dsf`/`.dff`) is **not** a `lofty` [`FileType`]; those files are read
///   by `lyra-dsd` and their codec (`DSD`) is set on the decoder-probe path, not
///   here. [`read_tags`] returns [`LyraError::UnsupportedFormat`] for them.
fn codec_from_filetype(ft: FileType) -> Option<String> {
    let s = match ft {
        FileType::Aac => "AAC",
        FileType::Aiff => "AIFF",
        FileType::Ape => "Monkey's Audio",
        FileType::Flac => "FLAC",
        FileType::Mpeg => "MP3",
        FileType::Mp4 => "MP4",
        FileType::Mpc => "Musepack",
        FileType::Opus => "Opus",
        FileType::Vorbis => "Vorbis",
        FileType::Speex => "Speex",
        FileType::Wav => "PCM",
        FileType::WavPack => "WavPack",
        FileType::Custom(name) => name,
        // `FileType` is `#[non_exhaustive]`; future variants fall back to Debug.
        other => return Some(format!("{other:?}")),
    };
    Some(s.to_string())
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
    use lofty::tag::{ItemValue, TagItem, TagType};

    #[test]
    fn codec_maps_known_filetypes() {
        assert_eq!(codec_from_filetype(FileType::Flac).as_deref(), Some("FLAC"));
        assert_eq!(codec_from_filetype(FileType::Wav).as_deref(), Some("PCM"));
        assert_eq!(codec_from_filetype(FileType::Mpeg).as_deref(), Some("MP3"));
        assert_eq!(codec_from_filetype(FileType::Aiff).as_deref(), Some("AIFF"));
        assert_eq!(codec_from_filetype(FileType::Opus).as_deref(), Some("Opus"));
        assert_eq!(
            codec_from_filetype(FileType::Vorbis).as_deref(),
            Some("Vorbis")
        );
        assert_eq!(
            codec_from_filetype(FileType::WavPack).as_deref(),
            Some("WavPack")
        );
        assert_eq!(codec_from_filetype(FileType::Mp4).as_deref(), Some("MP4"));
    }

    #[test]
    fn codec_custom_filetype_passes_name_through() {
        assert_eq!(
            codec_from_filetype(FileType::Custom("DSD")).as_deref(),
            Some("DSD")
        );
    }

    #[test]
    fn replaygain_and_disc_extracted_from_vorbis_tag() {
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::DiscNumber, "1/2".to_string());
        tag.insert_text(ItemKey::ReplayGainTrackGain, "-6.48 dB".to_string());
        tag.insert_text(ItemKey::ReplayGainAlbumGain, "-7.10 dB".to_string());
        tag.insert_text(ItemKey::ReplayGainTrackPeak, "0.988525".to_string());
        tag.insert_text(ItemKey::ReplayGainAlbumPeak, "0.999969".to_string());

        let f = TagFields::from_tag(&tag);
        assert_eq!(f.disc_number.as_deref(), Some("1/2"));
        assert_eq!(f.replaygain_track_gain.as_deref(), Some("-6.48 dB"));
        assert_eq!(f.replaygain_album_gain.as_deref(), Some("-7.10 dB"));
        assert_eq!(f.replaygain_track_peak.as_deref(), Some("0.988525"));
        assert_eq!(f.replaygain_album_peak.as_deref(), Some("0.999969"));

        // The native Vorbis keys also survive into the raw map.
        assert_eq!(f.tags.get("REPLAYGAIN_TRACK_GAIN"), Some("-6.48 dB"));
        assert_eq!(f.tags.get("DISCNUMBER"), Some("1/2"));
    }

    #[test]
    fn multi_valued_key_is_joined_in_raw_map() {
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.push(TagItem::new(
            ItemKey::TrackArtist,
            ItemValue::Text("Alice".to_string()),
        ));
        tag.push(TagItem::new(
            ItemKey::TrackArtist,
            ItemValue::Text("Bob".to_string()),
        ));

        let f = TagFields::from_tag(&tag);
        // Typed field keeps the primary (first) value, …
        assert_eq!(f.artist.as_deref(), Some("Alice"));
        // … while the raw map preserves every value joined with the separator.
        assert_eq!(f.tags.get("ARTIST"), Some("Alice; Bob"));
    }

    #[test]
    fn absent_replaygain_is_none() {
        let tag = Tag::new(TagType::VorbisComments);
        let f = TagFields::from_tag(&tag);
        assert!(f.replaygain_track_gain.is_none());
        assert!(f.disc_number.is_none());
    }

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
