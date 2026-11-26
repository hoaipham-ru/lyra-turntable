//! Sony DSF (`.dsf`) reader — uncompressed DSD.
//!
//! Layout: a `DSD ` header chunk (file size + ID3v2 pointer), a `fmt ` chunk
//! (version, channels, rate, 1-bit, sample count, block size), then a `data`
//! chunk. All sizes and fields are little-endian.
//!
//! DSF is **LSB-first** and samples are stored in per-channel blocks of
//! `block size` bytes (usually 4096), interleaved by block; the final block of
//! each channel is zero-padded. The decoder de-blocks into the common
//! channel-interleaved byte order.

use std::fs::File;
use std::path::Path;

use lyra_core::{DsdSpec, LyraError, Result, TagMap};

use crate::decoder::{DsdDecoder, Layout, Parsed};
use crate::util::{self, le_u32, le_u64};
use crate::{DsdContainer, DsdInfo, MetadataKind, MetadataRegion};

/// Result of parsing the DSF header chunks.
struct Dsf {
    spec: DsdSpec,
    total_frames: u64,
    data_start: u64,
    bytes_per_channel: u64,
    block_size: u64,
    metadata: Option<MetadataRegion>,
}

fn parse(file: &mut File, file_len: u64) -> Result<Dsf> {
    // DSD header (28) + fmt chunk (52) + data chunk header (12) = 92 bytes.
    let head = util::read_at(file, 0, 92)?;

    if &head[0..4] != b"DSD " {
        return Err(LyraError::UnsupportedFormat("not a DSF (DSD ) file".to_string()));
    }
    // head[4..12]  = DSD chunk size (28)
    // head[12..20] = total file size
    let meta_ptr = le_u64(&head, 20)?;

    if &head[28..32] != b"fmt " {
        return Err(LyraError::Decode("DSF missing fmt chunk".to_string()));
    }
    let fmt = &head[28..80];
    // fmt[0..8]   = fmt chunk size (52)
    let format_version = le_u32(fmt, 12)?;
    let format_id = le_u32(fmt, 16)?;
    // fmt[20..24] = channel type
    let channel_num = le_u32(fmt, 24)?;
    let sampling_freq = le_u32(fmt, 28)?;
    let bits_per_sample = le_u32(fmt, 32)?;
    let sample_count = le_u64(fmt, 36)?; // samples per channel
    let block_size = u64::from(le_u32(fmt, 44)?);

    if format_version != 1 {
        return Err(LyraError::UnsupportedFormat(format!(
            "unsupported DSF format version {format_version}"
        )));
    }
    if format_id != 0 {
        return Err(LyraError::UnsupportedFormat(
            "DSF format id is not 0 (DSD raw)".to_string(),
        ));
    }
    if bits_per_sample != 1 {
        return Err(LyraError::UnsupportedFormat(format!(
            "DSF bits-per-sample is {bits_per_sample}, expected 1"
        )));
    }
    if channel_num == 0 {
        return Err(LyraError::Decode("DSF has zero channels".to_string()));
    }
    if block_size == 0 {
        return Err(LyraError::Decode("DSF block size is zero".to_string()));
    }
    let rate = util::rate_from_hz(sampling_freq)?;
    let channels = u16::try_from(channel_num)
        .map_err(|_| LyraError::Decode("DSF channel count out of range".to_string()))?;

    if &head[80..84] != b"data" {
        return Err(LyraError::Decode("DSF missing data chunk".to_string()));
    }
    let data_chunk_size = le_u64(&head, 84)?; // includes the 12-byte chunk header
    let data_start = 92u64;
    let _data_size = data_chunk_size.saturating_sub(12);

    let bytes_per_channel = sample_count.div_ceil(8);

    let metadata = if meta_ptr != 0 && meta_ptr < file_len {
        Some(MetadataRegion {
            offset: meta_ptr,
            len: file_len - meta_ptr,
            kind: MetadataKind::Id3v2,
        })
    } else {
        None
    };

    Ok(Dsf {
        spec: DsdSpec { rate, channels, msb_first: false },
        total_frames: sample_count,
        data_start,
        bytes_per_channel,
        block_size,
        metadata,
    })
}

/// Open a DSF file as a decoder.
pub(crate) fn open(path: &Path) -> Result<DsdDecoder> {
    let mut file = File::open(path).map_err(|e| LyraError::Io(e.to_string()))?;
    let file_len = file.metadata().map_err(|e| LyraError::Io(e.to_string()))?.len();
    let dsf = parse(&mut file, file_len)?;
    let parsed = Parsed {
        spec: dsf.spec,
        total_frames: dsf.total_frames,
        data_start: dsf.data_start,
        bytes_per_channel: dsf.bytes_per_channel,
        layout: Layout::Dsf { block_size: dsf.block_size },
    };
    Ok(DsdDecoder::new(file, parsed, TagMap::new()))
}

/// Probe a DSF file without reading audio.
pub(crate) fn probe(path: &Path) -> Result<DsdInfo> {
    let mut file = File::open(path).map_err(|e| LyraError::Io(e.to_string()))?;
    let file_len = file.metadata().map_err(|e| LyraError::Io(e.to_string()))?.len();
    let dsf = parse(&mut file, file_len)?;
    Ok(DsdInfo {
        spec: dsf.spec,
        total_frames: dsf.total_frames,
        container: DsdContainer::Dsf,
        metadata: dsf.metadata,
    })
}
