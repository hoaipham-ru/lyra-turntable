//! Opening a track: decoder selection, cue virtual-track windows and DSD routing.
//!
//! A [`Source`] turns a [`TrackRef`] into playable audio in two phases:
//!
//! * [`Source::resolve`] — cheap. Parse a cue sheet (if any) and probe the image so
//!   the worker learns the virtual-track window, the device [`StreamSpec`] and the
//!   underlying image path **without a full decode**. The worker uses this during
//!   pre-roll to decide whether the next track can reuse the current decoder (two
//!   virtual tracks of the same cue image) or needs a fresh open.
//! * [`Source::open`] — opens the decoder, seeks it to the virtual-track start, and
//!   builds the [`Delivery`] (PCM pass-through, DoP packer, or DSD→PCM).
//!
//! PCM goes straight through; DSD is routed by [`crate::routing`] to native ASIO,
//! DoP, or DSD→PCM. The worker never re-opens a decoder for adjacent virtual tracks
//! that share an image — it carries the one decoder across the join.

use std::path::{Path, PathBuf};

use lyra_core::{
    Decoder, DsdSpec, LyraError, OutputMode, PcmSpec, Result, StreamSpec, TagMap, TrackInfo,
    TrackRef,
};
use lyra_dsd::{DopPacker, DsdToPcm, TargetRate};
use lyra_library::parse_cue;
use lyra_output::DeviceId;

use crate::routing::{choose_dsd_delivery, DsdDelivery};

/// How the worker converts decoded bytes to the device container.
pub(crate) enum Delivery {
    /// Decoded PCM is already in the device's sample domain; only container
    /// padding (if any) is applied.
    Pcm,
    /// DSD is decimated to 24-bit PCM (a processed path; not bit-perfect).
    DsdToPcm(DsdToPcm),
    /// DSD is packed into DoP 24-bit words (bit-exact DSD transport).
    Dop {
        /// The packer carrying the 0x05/0xFA marker state across buffers.
        packer: DopPacker,
        /// The PCM spec describing the packed DoP words (for padding).
        word_spec: PcmSpec,
    },
}

impl Delivery {
    /// Clear any converter/packer state at a seek or non-contiguous join, so no
    /// stale filter history or DoP marker leaks across the discontinuity.
    pub(crate) fn reset(&mut self) {
        match self {
            Delivery::Pcm => {}
            Delivery::DsdToPcm(conv) => conv.reset(),
            Delivery::Dop { packer, .. } => packer.reset(),
        }
    }
}

/// Cheap, decode-free description of a track: its window, specs and image path.
///
/// `virtual_start`/`virtual_end` are **absolute source frames in the image file**:
/// for a whole file that is `0..total_frames`; for a cue virtual track it is the
/// track's `[start_sampleframe, end_sampleframe)`. The worker reports position
/// relative to `virtual_start` (so a virtual track starts at frame 0).
#[derive(Clone)]
pub(crate) struct Resolved {
    /// Absolute path of the underlying image file, as a stable reuse key.
    pub image_key: String,
    /// First source frame of the virtual track (0 for a whole file).
    pub virtual_start: u64,
    /// Exclusive end source frame, if known (`None` means read to EOF).
    pub virtual_end: Option<u64>,
    /// Native decoded format of the image.
    pub source_spec: StreamSpec,
    /// The [`StreamSpec`] to request from the device.
    pub device_spec: StreamSpec,
    /// `true` when the delivery will alter samples (DSD→PCM).
    pub processed: bool,
    /// Track info to emit on [`lyra_core::EngineEvent::Track`]; `duration_frames`
    /// is the virtual-track length.
    pub info: TrackInfo,
}

impl Resolved {
    /// Virtual-track length in source frames, if both ends are known.
    pub(crate) fn length(&self) -> Option<u64> {
        self.virtual_end.map(|e| e.saturating_sub(self.virtual_start))
    }
}

/// An opened decoder and its delivery, positioned at the virtual-track start.
pub(crate) struct Opened {
    /// The source decoder (PCM or DSD), already seeked to `virtual_start`.
    pub decoder: Box<dyn Decoder>,
    /// How decoded bytes become device bytes.
    pub delivery: Delivery,
    /// The PCM spec of the bytes the delivery produces (input to padding).
    pub produced_pcm: Option<PcmSpec>,
}

/// Opens a [`TrackRef`] in two phases. Boxed so tests inject a fake.
pub(crate) trait Source: Send {
    /// Resolve the track's window and specs without a full decode.
    fn resolve(&mut self, track: &TrackRef) -> Result<Resolved>;
    /// Open the decoder for a resolved track, seeked to its virtual start.
    fn open(&mut self, track: &TrackRef, resolved: &Resolved) -> Result<Opened>;
}

/// Boxed [`Source`] handed to the worker.
pub(crate) type SourceBox = Box<dyn Source>;

/// Build the real decoder-backed source.
///
/// `device`/`mode` decide DSD routing (device caps are probed lazily, only when a
/// DSD file is opened, so PCM playback never touches the device twice).
pub(crate) fn real_source(device: DeviceId, mode: OutputMode) -> SourceBox {
    Box::new(RealSource { device, mode })
}

/// The real [`Source`]: `lyra_decode` for PCM, `lyra_dsd` for DSD, `lyra_library`
/// for cue sheets.
struct RealSource {
    device: DeviceId,
    mode: OutputMode,
}

impl Source for RealSource {
    fn resolve(&mut self, track: &TrackRef) -> Result<Resolved> {
        match cue_binding(track) {
            Some((cue_path, index)) => self.resolve_cue(track, &cue_path, index),
            None => self.resolve_plain(track),
        }
    }

    fn open(&mut self, _track: &TrackRef, resolved: &Resolved) -> Result<Opened> {
        let image = Path::new(&resolved.image_key);
        if is_dsd_path(image) {
            self.open_dsd(image, resolved)
        } else {
            open_pcm_decoder(image, resolved.virtual_start)
        }
    }
}

impl RealSource {
    /// Resolve a whole-file (non-cue) track.
    fn resolve_plain(&mut self, track: &TrackRef) -> Result<Resolved> {
        let path = Path::new(&track.path);
        let image_key = abs_key(path);
        if is_dsd_path(path) {
            // DSD cannot be probed by lyra_decode; open the reader to read its spec.
            let decoder = lyra_dsd::open(path)?;
            let source_spec = decoder.spec();
            let StreamSpec::Dsd(dsd) = source_spec else {
                return Err(LyraError::Decode("DSD reader returned a non-DSD spec".into()));
            };
            let total = decoder.total_frames();
            let (device_spec, processed) = self.dsd_target(dsd)?;
            let info = track_info(track, decoder.tags(), source_spec, total, None, None, None);
            Ok(Resolved {
                image_key,
                virtual_start: 0,
                virtual_end: total,
                source_spec,
                device_spec,
                processed,
                info,
            })
        } else {
            let probe = lyra_decode::probe(path)?;
            let source_spec = probe.spec;
            let total = probe.total_frames;
            let info = track_info(track, &probe.tags, source_spec, total, None, None, None);
            Ok(Resolved {
                image_key,
                virtual_start: 0,
                virtual_end: total,
                source_spec,
                device_spec: source_spec,
                processed: false,
                info,
            })
        }
    }

    /// Resolve a cue virtual track: parse the sheet, pick the sub-track, probe the
    /// referenced image for its spec.
    fn resolve_cue(&mut self, track: &TrackRef, cue_path: &Path, index: u32) -> Result<Resolved> {
        let sheet = parse_cue(cue_path)?;
        let flat: Vec<_> = sheet.tracks().collect();
        let (file, ctrack) = flat.get(index as usize).ok_or_else(|| {
            LyraError::InvalidState(format!(
                "cue {} has no sub-track {index} (it has {})",
                cue_path.display(),
                flat.len()
            ))
        })?;
        let start = ctrack.start_sampleframe.ok_or_else(|| {
            LyraError::InvalidState(format!(
                "cue {} sub-track {index}: sample rate unknown (image unreadable?)",
                cue_path.display()
            ))
        })?;
        let image = file.path.clone();
        let image_key = abs_key(&image);

        if is_dsd_path(&image) {
            return Err(LyraError::UnsupportedFormat(
                "cue sheets over DSD images are not supported".into(),
            ));
        }
        let probe = lyra_decode::probe(&image)?;
        let source_spec = probe.spec;
        let total = probe.total_frames;
        // Clamp the window to the image length: some cue sheets place INDEX times
        // past the end of a shorter file, which would seek out of range. A track
        // whose start is at/after EOF simply contributes no audio.
        let start = total.map_or(start, |t| start.min(t));
        let end = ctrack.end_sampleframe.or(total);
        let end = match (end, total) {
            (Some(e), Some(t)) => Some(e.min(t).max(start)),
            (Some(e), None) => Some(e.max(start)),
            (None, Some(t)) => Some(t.max(start)),
            (None, None) => None,
        };
        let title = ctrack.title.clone();
        let artist = ctrack
            .performer
            .clone()
            .or_else(|| sheet.disc_performer.clone());
        let album = sheet.disc_title.clone();
        let duration = end.map(|e| e.saturating_sub(start));
        let info = track_info(track, &probe.tags, source_spec, duration, title, artist, album);
        Ok(Resolved {
            image_key,
            virtual_start: start,
            virtual_end: end,
            source_spec,
            device_spec: source_spec,
            processed: false,
            info,
        })
    }

    /// Open a DSD image and build its delivery per the resolved target spec.
    fn open_dsd(&mut self, image: &Path, resolved: &Resolved) -> Result<Opened> {
        let decoder = lyra_dsd::open(image)?;
        let StreamSpec::Dsd(dsd) = decoder.spec() else {
            return Err(LyraError::Decode("DSD reader returned a non-DSD spec".into()));
        };
        let delivery = match choose_dsd_delivery(&self.caps(), self.mode) {
            DsdDelivery::ToPcm => Delivery::DsdToPcm(DsdToPcm::new(dsd, TargetRate::Pcm176_4)?),
            DsdDelivery::Dop => Delivery::Dop { packer: DopPacker::new(dsd), word_spec: dop_word_spec(dsd) },
            DsdDelivery::Native => {
                return Err(LyraError::UnsupportedFormat(
                    "native DSD over ASIO is not implemented on this device".into(),
                ))
            }
        };
        let produced_pcm = produced_pcm(&delivery, resolved.source_spec);
        Ok(Opened { decoder, delivery, produced_pcm })
    }

    /// The device spec and processed flag a DSD source would take, without opening.
    fn dsd_target(&self, dsd: DsdSpec) -> Result<(StreamSpec, bool)> {
        match choose_dsd_delivery(&self.caps(), self.mode) {
            DsdDelivery::ToPcm => {
                let conv = DsdToPcm::new(dsd, TargetRate::Pcm176_4)?;
                Ok((StreamSpec::Pcm(conv.out_spec()), true))
            }
            DsdDelivery::Dop => Ok((StreamSpec::Pcm(dop_word_spec(dsd)), false)),
            DsdDelivery::Native => Err(LyraError::UnsupportedFormat(
                "native DSD over ASIO is not implemented on this device".into(),
            )),
        }
    }

    /// Best-effort device capabilities for DSD routing.
    fn caps(&self) -> lyra_core::DeviceCaps {
        probe_caps(&self.device)
    }
}

/// Open a PCM image decoder and seek it to the virtual start.
fn open_pcm_decoder(image: &Path, start: u64) -> Result<Opened> {
    let mut decoder = lyra_decode::open(image, &lyra_decode::OpenOptions::default())?;
    if start > 0 {
        decoder.seek(start)?;
    }
    let produced_pcm = match decoder.spec() {
        StreamSpec::Pcm(p) => Some(p),
        StreamSpec::Dsd(_) => None,
    };
    Ok(Opened { decoder, delivery: Delivery::Pcm, produced_pcm })
}

/// The PCM spec the delivery emits (input to padding).
fn produced_pcm(delivery: &Delivery, source_spec: StreamSpec) -> Option<PcmSpec> {
    match delivery {
        Delivery::Pcm => match source_spec {
            StreamSpec::Pcm(p) => Some(p),
            StreamSpec::Dsd(_) => None,
        },
        Delivery::DsdToPcm(conv) => Some(conv.out_spec()),
        Delivery::Dop { word_spec, .. } => Some(*word_spec),
    }
}

/// Decide whether a [`TrackRef`] names a cue virtual track, returning the cue path
/// and the flat sub-track index.
///
/// Convention: a `.cue` path with `subtrack: Some(i)` is virtual track `i`. A plain
/// file path with `subtrack: Some(i)` is virtual track `i` of a sibling cue sharing
/// the file stem (`album.flac` → `album.cue`), if one exists. `subtrack: None` is a
/// whole file (cue sheets with `subtrack: None` are expanded into per-track refs by
/// the control thread before they reach here).
fn cue_binding(track: &TrackRef) -> Option<(PathBuf, u32)> {
    let path = Path::new(&track.path);
    if is_cue_path(path) {
        return track.subtrack.map(|i| (path.to_path_buf(), i));
    }
    let index = track.subtrack?;
    let sibling = path.with_extension("cue");
    sibling.is_file().then_some((sibling, index))
}

/// `.cue` (case-insensitive).
fn is_cue_path(path: &Path) -> bool {
    has_ext(path, &["cue"])
}

/// `.dsf` / `.dff` (case-insensitive) go to the DSD reader.
fn is_dsd_path(path: &Path) -> bool {
    has_ext(path, &["dsf", "dff"])
}

/// Case-insensitive extension test.
fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
        .unwrap_or(false)
}

/// A stable image key: the absolute path if it resolves, else the path as given.
fn abs_key(path: &Path) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned())
}

/// The PCM carrier spec for DoP words: DSD rate / 16, 24-in-3-byte samples.
fn dop_word_spec(dsd: DsdSpec) -> PcmSpec {
    PcmSpec {
        rate: dsd.rate.dop_pcm_rate(),
        channels: dsd.channels,
        bits_valid: 24,
        format: lyra_core::SampleFormat::S24,
    }
}

/// Best-effort device capabilities for routing. Any probe error (off-Windows, or no
/// endpoint) yields all-false caps, which routes DSD to the PCM path.
fn probe_caps(device: &DeviceId) -> lyra_core::DeviceCaps {
    match lyra_output::probe(device) {
        Ok(p) => lyra_core::DeviceCaps {
            name: p.info.name,
            exclusive: !p.exclusive_formats.is_empty(),
            shared: p.shared_mix_rate.is_some(),
            asio: false,
            dop: p.dop_capable,
            native_dsd: !p.native_dsd_rates.is_empty(),
        },
        Err(_) => lyra_core::DeviceCaps {
            name: String::new(),
            exclusive: false,
            shared: false,
            asio: false,
            dop: false,
            native_dsd: false,
        },
    }
}

/// Build [`TrackInfo`], preferring explicit (cue) metadata over file tags.
fn track_info(
    track: &TrackRef,
    tags: &TagMap,
    spec: StreamSpec,
    duration_frames: Option<u64>,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
) -> TrackInfo {
    TrackInfo {
        track: track.clone(),
        title: title.or_else(|| tag(tags, &["TITLE", "Title", "title"])),
        artist: artist.or_else(|| tag(tags, &["ARTIST", "Artist", "artist"])),
        album: album.or_else(|| tag(tags, &["ALBUM", "Album", "album"])),
        duration_frames,
        spec,
    }
}

/// First matching tag value, cloned.
fn tag(tags: &TagMap, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| tags.get(k).map(ToOwned::to_owned))
}
