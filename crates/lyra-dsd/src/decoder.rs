//! The DSD decoder shared by the DSF and DFF readers.
//!
//! Both containers deliver the same output interleave (one byte per channel, in
//! channel order, for each byte position in time) but store it differently on
//! disk, so the reader carries a [`Layout`] describing how to turn the stored
//! bytes back into that interleave.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use lyra_core::{Decoder, DsdSpec, LyraError, Result, StreamSpec, TagMap};

/// How sample bytes are laid out in the file.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Layout {
    /// DSDIFF: bytes are already channel-interleaved on disk.
    Dff,
    /// DSF: bytes are stored in per-channel blocks of `block_size` bytes, the
    /// blocks interleaved by channel (ch0 block, ch1 block, …, then the next
    /// group). The final block of each channel is zero-padded.
    Dsf {
        /// Block size per channel, in bytes (the DSF `block size` field, usually
        /// 4096).
        block_size: u64,
    },
}

/// Parsed parameters handed from a container parser to the decoder.
pub(crate) struct Parsed {
    pub spec: DsdSpec,
    pub total_frames: u64,
    pub data_start: u64,
    /// Valid DSD bytes per channel (`ceil(total_frames / 8)`).
    pub bytes_per_channel: u64,
    pub layout: Layout,
}

/// Target number of bytes per channel delivered per `read` call for DFF.
const DFF_READ_BYTES_PER_CHANNEL: u64 = 4096;

/// A streaming decoder over an uncompressed DSF or DFF file.
///
/// [`Decoder::read`] appends interleaved DSD bytes in the file's native bit order;
/// the bit order is reported by [`Decoder::spec`] so a consumer (e.g.
/// [`crate::DopPacker`]) can normalise it.
pub struct DsdDecoder {
    file: File,
    spec: DsdSpec,
    channels: usize,
    total_frames: u64,
    data_start: u64,
    bytes_per_channel: u64,
    layout: Layout,
    tags: TagMap,
    /// Next DSD sample (per channel) that `read` will deliver. Always a multiple
    /// of 8, so `position / 8` is the next byte index per channel.
    position: u64,
}

impl DsdDecoder {
    pub(crate) fn new(file: File, parsed: Parsed, tags: TagMap) -> Self {
        let channels = parsed.spec.channels as usize;
        DsdDecoder {
            file,
            spec: parsed.spec,
            channels,
            total_frames: parsed.total_frames,
            data_start: parsed.data_start,
            bytes_per_channel: parsed.bytes_per_channel,
            layout: parsed.layout,
            tags,
            position: 0,
        }
    }

    /// Read `len` bytes at `offset` into `buf[dst..dst + len]`.
    fn read_into(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| LyraError::Io(e.to_string()))?;
        self.file
            .read_exact(buf)
            .map_err(|e| LyraError::Io(e.to_string()))?;
        Ok(())
    }
}

impl Decoder for DsdDecoder {
    fn spec(&self) -> StreamSpec {
        StreamSpec::Dsd(self.spec)
    }

    fn total_frames(&self) -> Option<u64> {
        Some(self.total_frames)
    }

    fn seek(&mut self, frame: u64) -> Result<u64> {
        if frame > self.total_frames {
            return Err(LyraError::SeekOutOfRange(frame));
        }
        // DSD is byte-granular (8 samples/byte): land on the byte boundary at or
        // before the request. The returned frame is authoritative (Decoder docs).
        let landed = (frame / 8) * 8;
        self.position = landed;
        Ok(landed)
    }

    fn read(&mut self, out: &mut Vec<u8>) -> Result<usize> {
        let delivered = self.position / 8; // bytes per channel already delivered
        if delivered >= self.bytes_per_channel {
            return Ok(0);
        }
        let channels = self.channels;

        let n = match self.layout {
            Layout::Dff => {
                let n = (self.bytes_per_channel - delivered).min(DFF_READ_BYTES_PER_CHANNEL);
                // DFF is already interleaved: n bytes/channel = n*channels
                // contiguous bytes starting at the delivered position.
                let offset = self.data_start + delivered * channels as u64;
                let count = (n as usize) * channels;
                let base = out.len();
                out.resize(base + count, 0);
                self.read_into(offset, &mut out[base..])?;
                n
            }
            Layout::Dsf { block_size } => {
                let group = delivered / block_size;
                let off_in_block = delivered % block_size;
                // Deliver up to the end of valid data in the current block.
                let block_valid_end = ((group + 1) * block_size).min(self.bytes_per_channel);
                let n = block_valid_end - delivered;
                let n_usize = n as usize;

                // Read n valid bytes of each channel's block, then interleave.
                let mut scratch = vec![0u8; n_usize * channels];
                for ch in 0..channels {
                    let ch_off = self.data_start
                        + (group * channels as u64 + ch as u64) * block_size
                        + off_in_block;
                    self.read_into(ch_off, &mut scratch[ch * n_usize..(ch + 1) * n_usize])?;
                }
                out.reserve(n_usize * channels);
                for i in 0..n_usize {
                    for ch in 0..channels {
                        out.push(scratch[ch * n_usize + i]);
                    }
                }
                n
            }
        };

        self.position += n * 8;
        Ok((n * 8) as usize)
    }

    fn tags(&self) -> &TagMap {
        &self.tags
    }
}
