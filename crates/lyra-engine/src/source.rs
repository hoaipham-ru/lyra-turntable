//! Opening a track: decoder selection and DSD delivery routing.
//!
//! A [`SourceFactory`] turns a [`TrackRef`] into an [`OpenedSource`] — a decoder
//! plus the device [`StreamSpec`] to request and a [`Delivery`] describing how the
//! worker turns decoded bytes into device-format bytes. PCM goes straight through;
//! DSD is routed by [`crate::routing`] to native ASIO, DoP, or DSD→PCM. In P3.1 the
//! native and DoP transports are wired but only DSD→PCM runs on the dev laptop.

use std::path::Path;

use lyra_core::{
    Decoder, DsdSpec, LyraError, OutputMode, PcmSpec, Result, StreamSpec, TagMap, TrackInfo,
    TrackRef,
};
use lyra_dsd::{DopPacker, DsdToPcm, TargetRate};
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
    /// Clear any converter/packer state at a seek or track join, so no stale
    /// filter history or DoP marker leaks across the discontinuity.
    pub(crate) fn reset(&mut self) {
        match self {
            Delivery::Pcm => {}
            Delivery::DsdToPcm(conv) => conv.reset(),
            Delivery::Dop { packer, .. } => packer.reset(),
        }
    }
}

/// A decoder ready to play, with its routing decided.
pub(crate) struct OpenedSource {
    /// The source decoder (PCM or DSD).
    pub decoder: Box<dyn Decoder>,
    /// Resolved track info emitted on [`lyra_core::EngineEvent::Track`].
    pub info: TrackInfo,
    /// The native decoded format.
    pub source_spec: StreamSpec,
    /// The [`StreamSpec`] to request from the device.
    pub device_spec: StreamSpec,
    /// How decoded bytes become device bytes.
    pub delivery: Delivery,
}

impl OpenedSource {
    /// `true` when the delivery alters samples (DSD→PCM). DoP and plain PCM do not.
    pub(crate) fn is_processed(&self) -> bool {
        matches!(self.delivery, Delivery::DsdToPcm(_))
    }

    /// The PCM spec of the bytes the delivery produces, i.e. the input to padding.
    pub(crate) fn produced_pcm(&self) -> Option<PcmSpec> {
        match &self.delivery {
            Delivery::Pcm => match self.source_spec {
                StreamSpec::Pcm(p) => Some(p),
                StreamSpec::Dsd(_) => None,
            },
            Delivery::DsdToPcm(conv) => Some(conv.out_spec()),
            Delivery::Dop { word_spec, .. } => Some(*word_spec),
        }
    }
}

/// Opens a [`TrackRef`] into an [`OpenedSource`]. Boxed so tests inject a fake.
pub(crate) type SourceFactory = Box<dyn FnMut(&TrackRef) -> Result<OpenedSource> + Send>;

/// Build the real decoder-backed source factory.
///
/// `device`/`mode` decide DSD routing (the device caps are probed lazily, only
/// when a DSD file is opened, so PCM playback never touches the device twice).
pub(crate) fn real_source_factory(device: DeviceId, mode: OutputMode) -> SourceFactory {
    Box::new(move |track: &TrackRef| open_track(track, &device, mode))
}

/// Open one track, routing by container.
fn open_track(track: &TrackRef, device: &DeviceId, mode: OutputMode) -> Result<OpenedSource> {
    let path = Path::new(&track.path);
    if is_dsd_path(path) {
        open_dsd(track, path, device, mode)
    } else {
        open_pcm(track, path)
    }
}

/// `.dsf` / `.dff` (case-insensitive) go to the DSD reader.
fn is_dsd_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("dsf") || e.eq_ignore_ascii_case("dff"))
        .unwrap_or(false)
}

/// Open a PCM container (FLAC/WAV/…) straight through to the device.
fn open_pcm(track: &TrackRef, path: &Path) -> Result<OpenedSource> {
    let decoder = lyra_decode::open(path, &lyra_decode::OpenOptions::default())?;
    let source_spec = decoder.spec();
    let info = track_info(track, &*decoder, source_spec);
    Ok(OpenedSource {
        source_spec,
        device_spec: source_spec,
        delivery: Delivery::Pcm,
        info,
        decoder,
    })
}

/// Open a DSD container and route it per the device capabilities (ADR-009).
fn open_dsd(
    track: &TrackRef,
    path: &Path,
    device: &DeviceId,
    mode: OutputMode,
) -> Result<OpenedSource> {
    let decoder = lyra_dsd::open(path)?;
    let source_spec = decoder.spec();
    let StreamSpec::Dsd(dsd) = source_spec else {
        return Err(LyraError::Decode("DSD reader returned a non-DSD spec".into()));
    };
    let info = track_info(track, &*decoder, source_spec);

    let caps = probe_caps(device);
    let (device_spec, delivery) = match choose_dsd_delivery(&caps, mode) {
        DsdDelivery::ToPcm => {
            let conv = DsdToPcm::new(dsd, TargetRate::Pcm176_4)?;
            (StreamSpec::Pcm(conv.out_spec()), Delivery::DsdToPcm(conv))
        }
        DsdDelivery::Dop => {
            let word_spec = dop_word_spec(dsd);
            (
                StreamSpec::Pcm(word_spec),
                Delivery::Dop { packer: DopPacker::new(dsd), word_spec },
            )
        }
        DsdDelivery::Native => {
            // Native 1-bit transport to an ASIO driver is wired by routing but the
            // sink side lands in P3.3; P3.1 has no such device.
            return Err(LyraError::UnsupportedFormat(
                "native DSD over ASIO is not implemented until P3.3".into(),
            ));
        }
    };

    Ok(OpenedSource { decoder, info, source_spec, device_spec, delivery })
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

/// Best-effort device capabilities for routing. Any probe error (e.g. off-Windows,
/// or no endpoint) yields all-false caps, which routes DSD to the PCM path.
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

/// Build [`TrackInfo`] from the decoder's tags and length.
fn track_info(track: &TrackRef, decoder: &dyn Decoder, spec: StreamSpec) -> TrackInfo {
    let tags = decoder.tags();
    TrackInfo {
        track: track.clone(),
        title: tag(tags, &["TITLE", "Title", "title"]),
        artist: tag(tags, &["ARTIST", "Artist", "artist"]),
        album: tag(tags, &["ALBUM", "Album", "album"]),
        duration_frames: decoder.total_frames(),
        spec,
    }
}

/// First matching tag value, cloned.
fn tag(tags: &TagMap, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| tags.get(k).map(ToOwned::to_owned))
}
