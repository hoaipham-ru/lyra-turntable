//! The `symphonia`-backed decoder for FLAC and WAV, producing native PCM.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use lyra_core::{Decoder, LyraError, PcmSpec, Result, StreamSpec, TagMap};

use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{
    Decoder as SymDecoder, DecoderOptions, CODEC_TYPE_AAC, CODEC_TYPE_ALAC, CODEC_TYPE_FLAC, CODEC_TYPE_MP3,
    CODEC_TYPE_VORBIS,
};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::sample::SampleFormat as SymSampleFormat;

use crate::flac_sanitize;
use crate::target::{self, Target};
use crate::{OpenOptions, ProbeInfo};

/// A decoder over a FLAC or WAV stream.
pub struct FlacWavDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn SymDecoder>,
    track_id: u32,
    spec: PcmSpec,
    total: Option<u64>,
    tags: TagMap,
    codec: &'static str,
    target: Target,
    channels: usize,
    /// Next source frame index that `read` will deliver.
    position: u64,
    /// Frames to discard from decoded output after a seek (for sample accuracy).
    pending_skip: u64,
    /// Set once the stream has ended; further reads return `0`.
    ended: bool,
}

// `Box<dyn FormatReader>` and `Box<dyn SymDecoder>` are `Send`, so the whole
// decoder is `Send` as the `Decoder` contract requires.

impl FlacWavDecoder {
    /// Open from an arbitrary media source, sniffing FLAC/WAV via `hint` + content.
    fn build(
        source: Box<dyn MediaSource>,
        hint: &Hint,
        opts: &OpenOptions,
    ) -> Result<FlacWavDecoder> {
        let mss = MediaSourceStream::new(source, MediaSourceStreamOptions::default());

        let probed = symphonia::default::get_probe()
            .format(hint, mss, &FormatOptions::default(), &MetadataOptions::default())
            .map_err(map_open_err)?;

        let mut format = probed.format;
        let mut probed_meta = probed.metadata;

        let track = format
            .default_track()
            .or_else(|| format.tracks().first())
            .ok_or_else(|| LyraError::Decode("no audio track in file".to_string()))?;
        let track_id = track.id;
        let params = track.codec_params.clone();

        let codec = match params.codec {
            c if c == CODEC_TYPE_FLAC => "flac",
            c if c == CODEC_TYPE_MP3 => "mp3",
            c if c == CODEC_TYPE_AAC => "aac",
            c if c == CODEC_TYPE_ALAC => "alac",
            c if c == CODEC_TYPE_VORBIS => "vorbis",
            _ => "wav",
        };

        let rate = params
            .sample_rate
            .ok_or_else(|| LyraError::Decode("missing sample rate".to_string()))?;
        let channels = params
            .channels
            .ok_or_else(|| LyraError::Decode("missing channel layout".to_string()))?
            .count();
        if channels == 0 {
            return Err(LyraError::Decode("zero channels".to_string()));
        }

        let is_float = matches!(params.sample_format, Some(SymSampleFormat::F32) | Some(SymSampleFormat::F64));
        // MP3/AAC/Vorbis have no bit depth: their decoders output f32. Quantise those
        // to 24-bit integers (see write_block) rather than guessing 16.
        let lossy = matches!(codec, "mp3" | "aac" | "vorbis");
        let bits_valid: u8 = params
            .bits_per_sample
            .map(|b| b.min(32) as u8)
            .unwrap_or(if is_float { 32 } else if lossy { 24 } else { 16 });

        let target = if is_float {
            Target::float32()
        } else {
            Target::choose_int(bits_valid, opts.container_24bit)
        };

        let spec = PcmSpec {
            rate,
            channels: channels as u16,
            bits_valid: target.bits_valid,
            format: target.format,
        };

        let decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .map_err(map_open_err)?;

        // Collect tags from container metadata and any pre-stream metadata (e.g.
        // an ID3 tag ahead of the stream).
        let mut tags = TagMap::new();
        if let Some(meta) = probed_meta.get() {
            if let Some(rev) = meta.current() {
                collect_tags(rev, &mut tags);
            }
        }
        if let Some(rev) = format.metadata().current() {
            collect_tags(rev, &mut tags);
        }

        let total = params.n_frames;

        Ok(FlacWavDecoder {
            format,
            decoder,
            track_id,
            spec,
            total,
            tags,
            codec,
            target,
            channels,
            position: 0,
            pending_skip: 0,
            ended: false,
        })
    }

    pub(crate) fn probe_info(&self) -> ProbeInfo {
        ProbeInfo {
            spec: StreamSpec::Pcm(self.spec),
            total_frames: self.total,
            codec: self.codec,
            tags: self.tags.clone(),
        }
    }
}

impl Decoder for FlacWavDecoder {
    fn spec(&self) -> StreamSpec {
        StreamSpec::Pcm(self.spec)
    }

    fn total_frames(&self) -> Option<u64> {
        self.total
    }

    fn seek(&mut self, frame: u64) -> Result<u64> {
        if let Some(total) = self.total {
            if frame > total {
                return Err(LyraError::SeekOutOfRange(frame));
            }
            if frame == total {
                // Seek exactly to end: next read is EOF.
                self.position = frame;
                self.pending_skip = 0;
                self.ended = true;
                return Ok(frame);
            }
        }

        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::TimeStamp { ts: frame, track_id: self.track_id },
            )
            .map_err(|e| match e {
                SymError::SeekError(_) => LyraError::SeekOutOfRange(frame),
                other => LyraError::Decode(format!("seek to frame {frame} failed: {other}")),
            })?;

        // Accurate mode always lands at or before the request; decode-and-discard
        // the difference so the next read starts exactly at `frame`.
        self.pending_skip = frame.saturating_sub(seeked.actual_ts);
        self.position = frame;
        self.ended = false;
        self.decoder.reset();
        Ok(frame)
    }

    fn read(&mut self, out: &mut Vec<u8>) -> Result<usize> {
        if self.ended {
            return Ok(0);
        }

        let channels = self.channels;
        let target = self.target;
        let bits_valid = u32::from(self.spec.bits_valid);

        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.ended = true;
                    return Ok(0);
                }
                Err(SymError::ResetRequired) => {
                    self.ended = true;
                    return Ok(0);
                }
                Err(e) => {
                    return Err(LyraError::Decode(format!(
                        "read error at frame {}: {e}",
                        self.position
                    )));
                }
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.ended = true;
                    return Ok(0);
                }
                Err(e) => {
                    return Err(LyraError::Decode(format!(
                        "decode error at frame {}: {e}",
                        self.position
                    )));
                }
            };

            let frames = decoded.frames();
            let skip = (self.pending_skip as usize).min(frames);
            self.pending_skip -= skip as u64;
            if skip == frames {
                // Whole packet consumed by the post-seek discard; keep going.
                continue;
            }

            let produced = write_block(out, &decoded, channels, skip, target, bits_valid);
            self.position += produced as u64;
            return Ok(produced);
        }
    }

    fn tags(&self) -> &TagMap {
        &self.tags
    }
}

/// Interleave and pack frames `[skip..]` of `abr` into `out`; returns frames written.
fn write_block(
    out: &mut Vec<u8>,
    abr: &AudioBufferRef<'_>,
    channels: usize,
    skip: usize,
    target: Target,
    bits_valid: u32,
) -> usize {
    macro_rules! planar_int {
        ($buf:expr, $bufbits:expr, $conv:expr) => {{
            let buf = $buf;
            let frames = buf.frames();
            let down = ($bufbits as u32).saturating_sub(bits_valid);
            let mut produced = 0usize;
            for f in skip..frames {
                for ch in 0..channels {
                    let raw: i32 = $conv(buf.chan(ch)[f]);
                    target::push_int(out, raw >> down, target);
                }
                produced += 1;
            }
            produced
        }};
    }

    match abr {
        AudioBufferRef::U8(buf) => planar_int!(buf, 8, |s: u8| i32::from(s) - 128),
        AudioBufferRef::S16(buf) => planar_int!(buf, 16, |s: i16| i32::from(s)),
        AudioBufferRef::S24(buf) => planar_int!(buf, 24, |s: symphonia::core::sample::i24| s.inner()),
        AudioBufferRef::S32(buf) => planar_int!(buf, 32, |s: i32| s),
        AudioBufferRef::F32(buf) => {
            let frames = buf.frames();
            let mut produced = 0usize;
            // Writing raw f32 bytes into an integer stream (the 1.0.1 MP3 bug) is loud
            // noise: only a float target takes f32 as-is.
            let full = (1i64 << (bits_valid.clamp(2, 32) - 1)) as f64;
            for f in skip..frames {
                for ch in 0..channels {
                    let x = buf.chan(ch)[f];
                    if target.is_float {
                        out.extend_from_slice(&x.to_le_bytes());
                    } else {
                        target::push_int(out, f32_to_int(x, full), target);
                    }
                }
                produced += 1;
            }
            produced
        }
        AudioBufferRef::F64(buf) => {
            let frames = buf.frames();
            let mut produced = 0usize;
            let full = (1i64 << (bits_valid.clamp(2, 32) - 1)) as f64;
            for f in skip..frames {
                for ch in 0..channels {
                    let x = buf.chan(ch)[f];
                    if target.is_float {
                        out.extend_from_slice(&(x as f32).to_le_bytes());
                    } else {
                        target::push_int(out, f32_to_int(x as f32, full), target);
                    }
                }
                produced += 1;
            }
            produced
        }
        // Remaining variants (U16/U24/U32/S8/F64) are not produced by the FLAC or
        // PCM-WAV decoders in P1; treat any frames as unconvertible and skip them
        // rather than panic.
        _ => 0,
    }
}

/// Float sample in [-1, 1) to a signed integer at full scale `full` (= 2^(bits-1)),
/// rounded and clipped to the integer range.
fn f32_to_int(x: f32, full: f64) -> i32 {
    let v = (f64::from(x) * full).round();
    v.clamp(-full, full - 1.0) as i32
}

#[cfg(test)]
mod float_conversion_tests {
    use super::f32_to_int;

    #[test]
    fn scales_rounds_and_clips() {
        let full = (1i64 << 23) as f64; // 24-bit
        assert_eq!(f32_to_int(0.0, full), 0);
        assert_eq!(f32_to_int(0.5, full), 1 << 22);
        assert_eq!(f32_to_int(-1.0, full), -(1 << 23));
        assert_eq!(f32_to_int(1.0, full), (1 << 23) - 1); // clipped, no wrap
        assert_eq!(f32_to_int(3.0, full), (1 << 23) - 1);
        assert_eq!(f32_to_int(-3.0, full), -(1 << 23));
    }
}

fn collect_tags(rev: &symphonia::core::meta::MetadataRevision, tags: &mut TagMap) {
    for tag in rev.tags() {
        if tag.key.is_empty() {
            continue;
        }
        tags.insert(tag.key.to_uppercase(), tag.value.to_string());
    }
}

fn map_open_err(e: SymError) -> LyraError {
    match e {
        SymError::Unsupported(msg) => LyraError::UnsupportedFormat(msg.to_string()),
        SymError::IoError(err) => LyraError::Io(err.to_string()),
        other => LyraError::Decode(other.to_string()),
    }
}

/// Open `path` as a FLAC or WAV file, producing native PCM.
pub fn open(path: &Path, opts: &OpenOptions) -> Result<FlacWavDecoder> {
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    // Primary path: stream straight from the file.
    let file = fs::File::open(path).map_err(|e| LyraError::Io(e.to_string()))?;
    match FlacWavDecoder::build(Box::new(file), &hint, opts) {
        Ok(dec) => Ok(dec),
        Err(primary) => {
            // Fallback for FLAC: tolerate a leading ID3 tag and broken metadata
            // blocks by rebuilding a minimal stream, then retry.
            if looks_like_flac(path) {
                let bytes = fs::read(path).map_err(|e| LyraError::Io(e.to_string()))?;
                if let Ok(clean) = flac_sanitize::sanitize(&bytes) {
                    let mut fhint = Hint::new();
                    fhint.with_extension("flac");
                    return FlacWavDecoder::build(Box::new(Cursor::new(clean)), &fhint, opts);
                }
            }
            Err(primary)
        }
    }
}

fn looks_like_flac(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("flac"))
        == Some(true)
    {
        return true;
    }
    // Sniff: `fLaC` marker, possibly behind a leading ID3 tag.
    if let Ok(prefix) = read_prefix(path, 10) {
        let start = flac_sanitize::id3v2_len(&prefix);
        if start == 0 {
            return prefix.starts_with(b"fLaC");
        }
        // ID3 present: assume FLAC (the only format we front-skip ID3 for).
        return true;
    }
    false
}

fn read_prefix(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = fs::File::open(path)?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf)?;
    buf.truncate(read);
    Ok(buf)
}
