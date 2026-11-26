//! Philips DSDIFF (`.dff`) reader — uncompressed `DSD ` sound data only.
//!
//! Layout: a `FRM8` chunk of form type `DSD `, holding `FVER`, a `PROP` property
//! chunk (`SND ` with `FS `, `CHNL`, `CMPR`, …), the `DSD ` sound-data chunk, and
//! optionally `COMT`/`DIIN`/`ID3 ` metadata. All chunk sizes are 64-bit
//! big-endian, and each chunk is padded to an even byte boundary.
//!
//! DFF is **MSB-first** and the sound data is already channel-interleaved
//! (`L0 R0 L1 R1 …`, one byte = 8 samples per channel). DST-compressed data
//! (a `DST ` sound chunk, or `CMPR` naming `DST `) is rejected as unsupported.

use std::fs::File;
use std::path::Path;

use lyra_core::{DsdSpec, LyraError, Result, TagMap};

use crate::decoder::{DsdDecoder, Layout, Parsed};
use crate::util::{self, be_u16, be_u32, be_u64};
use crate::{DsdContainer, DsdInfo, MetadataKind, MetadataRegion};

/// Largest `PROP` chunk we will read into memory (guards against a bogus size).
const MAX_PROP: u64 = 1 << 20;

/// Result of parsing the DFF header chunks.
struct Dff {
    spec: DsdSpec,
    total_frames: u64,
    data_start: u64,
    bytes_per_channel: u64,
    metadata: Option<MetadataRegion>,
}

fn parse(file: &mut File, file_len: u64) -> Result<Dff> {
    let head = util::read_at(file, 0, 16)?;
    if &head[0..4] != b"FRM8" || &head[12..16] != b"DSD " {
        return Err(LyraError::UnsupportedFormat(
            "not a DSDIFF (FRM8/DSD ) file".to_string(),
        ));
    }
    let form_size = be_u64(&head, 4)?; // data size: form type (4) + nested chunks
    // Form data begins at offset 12; the 4-byte form type ("DSD ") was already
    // read in `head[12..16]`, so nested chunks span [16, 12 + form_size).
    let inner_end = form_size.saturating_add(12).min(file_len);

    let mut rate_hz: Option<u32> = None;
    let mut channels: Option<u16> = None;
    let mut compressed = false;
    let mut sound: Option<(u64, u64)> = None; // (data_start, data_size)
    let mut metadata: Option<MetadataRegion> = None;

    let mut pos = 16u64;
    while pos + 12 <= inner_end {
        let ch = match util::read_at(file, pos, 12) {
            Ok(b) => b,
            Err(_) => break, // tolerate trailing junk
        };
        let id = &ch[0..4];
        let size = be_u64(&ch, 4)?;
        let data_at = pos + 12;

        match id {
            b"PROP" => {
                if size > MAX_PROP {
                    return Err(LyraError::Decode("PROP chunk too large".to_string()));
                }
                let prop = util::read_at(file, data_at, size as usize)?;
                parse_prop(&prop, &mut rate_hz, &mut channels, &mut compressed)?;
            }
            b"DSD " => sound = Some((data_at, size)),
            b"DST " => {
                return Err(LyraError::UnsupportedFormat(
                    "DST-compressed DFF is not supported".to_string(),
                ))
            }
            b"COMT" if metadata.is_none() => {
                metadata = Some(MetadataRegion { offset: data_at, len: size, kind: MetadataKind::DffComment });
            }
            b"DIIN" if metadata.is_none() => {
                metadata = Some(MetadataRegion { offset: data_at, len: size, kind: MetadataKind::DffDiin });
            }
            b"ID3 " if metadata.is_none() => {
                metadata = Some(MetadataRegion { offset: data_at, len: size, kind: MetadataKind::Id3v2 });
            }
            _ => {}
        }

        // Advance past the data, honouring the even-boundary pad byte.
        pos = data_at + size + (size & 1);
    }

    if compressed {
        return Err(LyraError::UnsupportedFormat(
            "DST-compressed DFF is not supported".to_string(),
        ));
    }

    let rate = util::rate_from_hz(
        rate_hz.ok_or_else(|| LyraError::Decode("DFF missing FS (sample rate)".to_string()))?,
    )?;
    let channels = channels
        .ok_or_else(|| LyraError::Decode("DFF missing CHNL (channel count)".to_string()))?;
    if channels == 0 {
        return Err(LyraError::Decode("DFF has zero channels".to_string()));
    }
    let (data_start, data_size) =
        sound.ok_or_else(|| LyraError::Decode("DFF missing DSD sound chunk".to_string()))?;
    // Clamp the declared sound size to what the file actually holds.
    let data_size = data_size.min(file_len.saturating_sub(data_start));
    if data_size % channels as u64 != 0 {
        return Err(LyraError::Decode(
            "DFF sound data not a whole number of bytes per channel".to_string(),
        ));
    }
    let bytes_per_channel = data_size / channels as u64;
    let total_frames = bytes_per_channel * 8;

    Ok(Dff {
        spec: DsdSpec { rate, channels, msb_first: true },
        total_frames,
        data_start,
        bytes_per_channel,
        metadata,
    })
}

/// Parse the `PROP` chunk body (property type `SND ` plus nested chunks).
fn parse_prop(
    prop: &[u8],
    rate_hz: &mut Option<u32>,
    channels: &mut Option<u16>,
    compressed: &mut bool,
) -> Result<()> {
    if prop.len() < 4 || &prop[0..4] != b"SND " {
        return Err(LyraError::Decode("DFF PROP is not of type SND ".to_string()));
    }
    let mut p = 4usize;
    while p + 12 <= prop.len() {
        let id = &prop[p..p + 4];
        let size = be_u64(prop, p + 4)? as usize;
        let data_at = p + 12;
        let end = data_at.saturating_add(size);
        if end > prop.len() {
            break;
        }
        let data = &prop[data_at..end];
        match id {
            b"FS  " => *rate_hz = Some(be_u32(data, 0)?),
            b"CHNL" => *channels = Some(be_u16(data, 0)?),
            b"CMPR" => {
                // compressionType (4 bytes) then a counted ASCII name.
                if data.len() >= 4 && &data[0..4] == b"DST " {
                    *compressed = true;
                }
            }
            _ => {}
        }
        p = data_at + size + (size & 1);
    }
    Ok(())
}

/// Open a DFF file as a decoder.
pub(crate) fn open(path: &Path) -> Result<DsdDecoder> {
    let mut file = File::open(path).map_err(|e| LyraError::Io(e.to_string()))?;
    let file_len = file.metadata().map_err(|e| LyraError::Io(e.to_string()))?.len();
    let dff = parse(&mut file, file_len)?;
    let parsed = Parsed {
        spec: dff.spec,
        total_frames: dff.total_frames,
        data_start: dff.data_start,
        bytes_per_channel: dff.bytes_per_channel,
        layout: Layout::Dff,
    };
    Ok(DsdDecoder::new(file, parsed, TagMap::new()))
}

/// Probe a DFF file without reading audio.
pub(crate) fn probe(path: &Path) -> Result<DsdInfo> {
    let mut file = File::open(path).map_err(|e| LyraError::Io(e.to_string()))?;
    let file_len = file.metadata().map_err(|e| LyraError::Io(e.to_string()))?.len();
    let dff = parse(&mut file, file_len)?;
    Ok(DsdInfo {
        spec: dff.spec,
        total_frames: dff.total_frames,
        container: DsdContainer::Dff,
        metadata: dff.metadata,
    })
}
