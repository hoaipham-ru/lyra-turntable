//! ffmpeg/ffprobe-backed reference decode.
//!
//! For P0 the `decode` path produces the *reference* PCM by shelling out to
//! ffmpeg (ADR-007: ffmpeg is a third-opinion reference decoder; libFLAC is the
//! foobar2000 proxy, wired up in `golden.ps1`). Lyra's own symphonia/libFLAC
//! decoders arrive in P1 and are checked against these references.

use crate::pcm;
use anyhow::{anyhow, bail, Context, Result};
use lyra_core::{PcmSpec, SampleFormat};
use std::path::Path;
use std::process::Command;

/// What ffprobe reports about the first audio stream of a file.
#[derive(Debug, Clone)]
pub struct ProbeInfo {
    /// Sample rate in Hz.
    pub rate: u32,
    /// Channel count.
    pub channels: u16,
    /// Valid bits per sample as reported by the source.
    pub bits: u32,
}

/// Run `ffprobe` and extract rate/channels/bits for the first audio stream.
pub fn probe(input: &Path) -> Result<ProbeInfo> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=sample_rate,channels,bits_per_raw_sample,bits_per_sample,sample_fmt",
            "-of",
            "json",
        ])
        .arg(input)
        .output()
        .context("failed to spawn ffprobe (is ffmpeg on PATH?)")?;
    if !out.status.success() {
        bail!(
            "ffprobe failed on {}: {}",
            input.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).context("ffprobe JSON parse")?;
    let stream = v
        .get("streams")
        .and_then(|s| s.get(0))
        .ok_or_else(|| anyhow!("no audio stream in {}", input.display()))?;

    let rate = stream
        .get("sample_rate")
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| anyhow!("missing sample_rate"))?;
    let channels = stream
        .get("channels")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| anyhow!("missing channels"))? as u16;

    // bits_per_raw_sample is the authoritative valid bit depth for lossless
    // sources (FLAC 24-bit reports 24 here while sample_fmt is s32). Fall back
    // to bits_per_sample, then to the sample_fmt container width.
    let bits = stream
        .get("bits_per_raw_sample")
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&b| b > 0)
        .or_else(|| {
            stream
                .get("bits_per_sample")
                .and_then(serde_json::Value::as_u64)
                .map(|b| b as u32)
                .filter(|&b| b > 0)
        })
        .or_else(|| {
            stream
                .get("sample_fmt")
                .and_then(|x| x.as_str())
                .map(bits_from_sample_fmt)
        })
        .unwrap_or(16);

    Ok(ProbeInfo { rate, channels, bits })
}

fn bits_from_sample_fmt(fmt: &str) -> u32 {
    match fmt {
        "u8" | "u8p" => 8,
        "s16" | "s16p" => 16,
        "s32" | "s32p" | "flt" | "fltp" => 32,
        _ => 16,
    }
}

/// Decoded reference PCM plus its spec.
pub struct Decoded {
    /// Full stream spec (rate, channels, valid bits, chosen container format).
    pub spec: PcmSpec,
    /// Raw interleaved little-endian PCM payload (no container header).
    pub pcm: Vec<u8>,
}

/// Decode `input` to raw interleaved PCM via ffmpeg, choosing the container
/// format from the source valid bit depth (`pcm::format_for_bits`).
pub fn decode_raw(input: &Path) -> Result<Decoded> {
    let info = probe(input)?;
    let format = pcm::format_for_bits(info.bits);
    let codec = pcm::ffmpeg_pcm_codec(format);
    let fmt_flag = match format {
        SampleFormat::S16 => "s16le",
        SampleFormat::S24 => "s24le",
        SampleFormat::S32 | SampleFormat::F32 => "s32le",
    };

    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(input)
        .args(["-map", "a:0", "-c:a", codec, "-f", fmt_flag, "-"])
        .output()
        .context("failed to spawn ffmpeg (is it on PATH?)")?;
    if !out.status.success() {
        bail!(
            "ffmpeg decode failed on {}: {}",
            input.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    let spec = PcmSpec {
        rate: info.rate,
        channels: info.channels,
        bits_valid: u8::try_from(info.bits).unwrap_or(32),
        format,
    };
    Ok(Decoded { spec, pcm: out.stdout })
}

/// Build a canonical 44-byte WAV header for the given spec and payload length.
///
/// # Errors
/// A canonical WAV header stores the `data` chunk size and the `RIFF` size as
/// 32-bit little-endian fields. Returns an error if `data_len` (or `data_len +
/// 36`) does not fit in a `u32`, rather than silently truncating/wrapping and
/// emitting a corrupt header for a payload >= 4 GiB.
pub fn wav_header(spec: &PcmSpec, data_len: usize) -> Result<Vec<u8>> {
    let data_len_u32 = u32::try_from(data_len).map_err(|_| {
        anyhow!(
            "WAV payload too large for a 32-bit `data` chunk: {data_len} bytes (max {})",
            u32::MAX
        )
    })?;
    let riff_len = data_len_u32.checked_add(36).ok_or_else(|| {
        anyhow!(
            "WAV payload too large for a 32-bit `RIFF` size: {data_len} bytes (max {})",
            u32::MAX - 36
        )
    })?;
    let channels = u32::from(spec.channels);
    let container_bits = pcm::container_bits(spec.format);
    let block_align = channels * (container_bits / 8);
    let byte_rate = spec.rate * block_align;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&riff_len.to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM
    h.extend_from_slice(&(spec.channels).to_le_bytes());
    h.extend_from_slice(&spec.rate.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&(block_align as u16).to_le_bytes());
    h.extend_from_slice(&(container_bits as u16).to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len_u32.to_le_bytes());
    Ok(h)
}
