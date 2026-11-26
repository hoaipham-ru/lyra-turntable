//! Small byte-reading helpers shared by the DSF and DFF parsers.
//!
//! All helpers are bounds-checked and return [`LyraError`] rather than panicking,
//! so malformed input never aborts the process (`#![forbid(unsafe_code)]` keeps
//! the whole crate panic-safe on bad data).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use lyra_core::{DsdRate, LyraError, Result};

/// Read the first `n` bytes of `path` (fewer if the file is shorter).
pub(crate) fn read_prefix(path: &Path, n: usize) -> Result<Vec<u8>> {
    let mut f = File::open(path).map_err(|e| LyraError::Io(e.to_string()))?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf).map_err(|e| LyraError::Io(e.to_string()))?;
    buf.truncate(read);
    Ok(buf)
}

/// Seek to `offset` and read exactly `len` bytes.
pub(crate) fn read_at(file: &mut File, offset: u64, len: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| LyraError::Io(e.to_string()))?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf)
        .map_err(|e| LyraError::Io(e.to_string()))?;
    Ok(buf)
}

/// Read a `u16` big-endian from `b` at `off`, or error if out of range.
pub(crate) fn be_u16(b: &[u8], off: usize) -> Result<u16> {
    let s = b
        .get(off..off + 2)
        .ok_or_else(|| LyraError::Decode("truncated header (u16)".to_string()))?;
    Ok(u16::from_be_bytes([s[0], s[1]]))
}

/// Read a `u32` big-endian from `b` at `off`, or error if out of range.
pub(crate) fn be_u32(b: &[u8], off: usize) -> Result<u32> {
    let s = b
        .get(off..off + 4)
        .ok_or_else(|| LyraError::Decode("truncated header (u32 be)".to_string()))?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// Read a `u64` big-endian from `b` at `off`, or error if out of range.
pub(crate) fn be_u64(b: &[u8], off: usize) -> Result<u64> {
    let s = b
        .get(off..off + 8)
        .ok_or_else(|| LyraError::Decode("truncated header (u64 be)".to_string()))?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Ok(u64::from_be_bytes(a))
}

/// Read a `u32` little-endian from `b` at `off`, or error if out of range.
pub(crate) fn le_u32(b: &[u8], off: usize) -> Result<u32> {
    let s = b
        .get(off..off + 4)
        .ok_or_else(|| LyraError::Decode("truncated header (u32 le)".to_string()))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Read a `u64` little-endian from `b` at `off`, or error if out of range.
pub(crate) fn le_u64(b: &[u8], off: usize) -> Result<u64> {
    let s = b
        .get(off..off + 8)
        .ok_or_else(|| LyraError::Decode("truncated header (u64 le)".to_string()))?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Ok(u64::from_le_bytes(a))
}

/// Map a raw DSD bit rate in Hz to a [`DsdRate`], or error for an unknown rate.
pub(crate) fn rate_from_hz(hz: u32) -> Result<DsdRate> {
    match hz {
        2_822_400 => Ok(DsdRate::Dsd64),
        5_644_800 => Ok(DsdRate::Dsd128),
        11_289_600 => Ok(DsdRate::Dsd256),
        22_579_200 => Ok(DsdRate::Dsd512),
        other => Err(LyraError::UnsupportedFormat(format!(
            "unsupported DSD rate {other} Hz"
        ))),
    }
}
