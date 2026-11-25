//! libFLAC cross-check (feature `libflac-check`, OFF by default).
//!
//! ADR-007: FLAC decoding is cross-checked against libFLAC — the decoder
//! foobar2000 itself ships — and if `symphonia` ever disagrees, libFLAC wins. This
//! module shells out to the `flac` CLI (installed via winget, not necessarily on
//! `PATH`) to produce a raw reference decode in the same native, LSB-justified,
//! little-endian signed layout as [`crate::open`]'s default (`Container24::Packed`)
//! target, so the two byte streams can be compared directly.
//!
//! The normal build never compiles this module, so it adds no external dependency;
//! it is a test-only hook.

use std::path::{Path, PathBuf};
use std::process::Command;

use lyra_core::{LyraError, Result};

/// The known winget install location of the `flac` CLI on the dev machine
/// (mirrors `tests/parity/golden.ps1`).
const WINGET_FLAC: &str = "C:\\Users\\LENOVO\\AppData\\Local\\Microsoft\\WinGet\\Packages\\Xiph.FLAC_Microsoft.Winget.Source_8wekyb3d8bbwe\\flac-1.5.0-win\\Win64\\flac.exe";

/// Locate the `flac` binary: `PATH` first, then the known winget path.
#[must_use]
pub fn flac_binary() -> Option<PathBuf> {
    // On PATH?
    if Command::new("flac").arg("--version").output().is_ok() {
        return Some(PathBuf::from("flac"));
    }
    let fallback = PathBuf::from(WINGET_FLAC);
    if fallback.exists() {
        return Some(fallback);
    }
    None
}

/// Decode `path` to raw interleaved PCM via libFLAC, in the narrowest native
/// container (little-endian, signed) — the same layout as the default decode
/// target. Returns the raw bytes.
///
/// # Errors
/// Returns [`LyraError::Decode`] if the `flac` binary is missing or the decode
/// fails.
pub fn decode_raw(path: &Path) -> Result<Vec<u8>> {
    let flac = flac_binary()
        .ok_or_else(|| LyraError::Decode("flac CLI not found (PATH or winget path)".to_string()))?;

    // Decode to a temp raw file under the OS temp dir, then read it back. Using a
    // file (not a pipe) keeps this robust across flac CLI versions.
    let mut out = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    out.push(format!("lyra_libflac_{stamp}.raw"));

    let status = Command::new(&flac)
        .arg("-d")
        .arg("--force-raw-format")
        .arg("--endian=little")
        .arg("--sign=signed")
        .arg("-f") // overwrite
        .arg("-o")
        .arg(&out)
        .arg("--") // stop option parsing; the input path follows
        .arg(path)
        .output()
        .map_err(|e| LyraError::Decode(format!("failed to run flac: {e}")))?;

    if !status.status.success() {
        let _ = std::fs::remove_file(&out);
        return Err(LyraError::Decode(format!(
            "flac -d failed: {}",
            String::from_utf8_lossy(&status.stderr)
        )));
    }

    let bytes = std::fs::read(&out).map_err(|e| LyraError::Io(e.to_string()))?;
    let _ = std::fs::remove_file(&out);
    Ok(bytes)
}

/// Compare `ours` against the libFLAC reference decode of `path`.
///
/// On any mismatch libFLAC is authoritative: returns [`LyraError::Decode`] naming
/// the first differing byte and the frame it falls in (given `bytes_per_frame`).
///
/// # Errors
/// Returns an error if the reference decode fails or the streams differ.
pub fn cross_check(path: &Path, ours: &[u8], bytes_per_frame: usize) -> Result<()> {
    let reference = decode_raw(path)?;
    if ours.len() != reference.len() {
        return Err(LyraError::Decode(format!(
            "libFLAC disagreement: length {} != reference {}",
            ours.len(),
            reference.len()
        )));
    }
    if let Some(i) = ours.iter().zip(reference.iter()).position(|(a, b)| a != b) {
        let frame = i.checked_div(bytes_per_frame).unwrap_or(0);
        return Err(LyraError::Decode(format!(
            "libFLAC disagreement at byte {i} (frame {frame}): ours={:#04x} reference={:#04x}",
            ours[i], reference[i]
        )));
    }
    Ok(())
}
