//! Read-only cover-art lookup (S06, P4.2).
//!
//! [`artwork`] returns the first usable cover image for a track:
//!
//! 1. an **embedded** picture read via `lofty` (the file is opened read-only and
//!    never decoded or modified), then
//! 2. a **folder image** next to the file — `folder.jpg`, `cover.jpg`,
//!    `cover.png` or `front.*` — in that order.
//!
//! # Robustness
//! Nothing here is fatal. A file `lofty` cannot parse, or an embedded picture that
//! is broken (two corpus FLACs carry a picture with **no MIME type**), is skipped
//! and the folder fallback is tried instead; if nothing is found the result is
//! simply `Ok(None)`. This mirrors the P1 "broken artwork is skipped with a
//! warning, never fatal" rule.

use std::path::Path;

use lofty::file::TaggedFileExt;
use lofty::read_from_path;

use lyra_core::Result;

/// A cover image: its MIME type and raw encoded bytes (JPEG/PNG/…).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artwork {
    /// The image MIME type, e.g. `image/jpeg` or `image/png`.
    pub mime: String,
    /// The raw encoded image bytes, exactly as stored.
    pub data: Vec<u8>,
}

/// File names searched for a folder image, in priority order. Matched
/// case-insensitively.
const FOLDER_NAMES: &[&str] = &[
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
    "front.jpg",
    "front.jpeg",
    "front.png",
];

/// Return cover art for the track at `path`, embedded first then a folder image,
/// or `None` if neither exists.
///
/// # Errors
/// Returns [`LyraError::Io`](lyra_core::LyraError::Io) only if a folder image is
/// found but cannot be read. An unreadable or unparseable audio file, or a broken
/// embedded picture, is not an error: it falls through to the folder lookup.
pub fn artwork(path: &Path) -> Result<Option<Artwork>> {
    if let Some(art) = embedded_artwork(path) {
        return Ok(Some(art));
    }
    folder_artwork(path)
}

/// Try to read a usable embedded picture. Returns `None` if the file cannot be
/// parsed or carries no picture with both a MIME type and non-empty data.
fn embedded_artwork(path: &Path) -> Option<Artwork> {
    let tagged = read_from_path(path).ok()?;
    for tag in tagged.tags() {
        for pic in tag.pictures() {
            // A broken embedded picture (no MIME type) or an empty payload is
            // skipped so a single bad image never hides a good fallback.
            let Some(mime) = pic.mime_type() else {
                continue;
            };
            let data = pic.data();
            if data.is_empty() {
                continue;
            }
            return Some(Artwork {
                mime: mime.as_str().to_string(),
                data: data.to_vec(),
            });
        }
    }
    None
}

/// Look for a folder image beside the file.
fn folder_artwork(path: &Path) -> Result<Option<Artwork>> {
    let Some(dir) = path.parent() else {
        return Ok(None);
    };
    // Read the directory once and match names case-insensitively, so `Folder.JPG`
    // and `cover.PNG` are found too.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(None);
    };
    let mut present: Vec<(String, std::path::PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            present.push((name.to_ascii_lowercase(), entry.path()));
        }
    }
    for wanted in FOLDER_NAMES {
        if let Some((_, p)) = present.iter().find(|(lower, _)| lower == wanted) {
            let data = std::fs::read(p).map_err(|e| {
                lyra_core::LyraError::Io(format!("reading cover image {}: {e}", p.display()))
            })?;
            return Ok(Some(Artwork {
                mime: mime_for(wanted).to_string(),
                data,
            }));
        }
    }
    Ok(None)
}

/// Guess a MIME type from a lower-cased file name's extension.
fn mime_for(name: &str) -> &'static str {
    if name.ends_with(".png") {
        "image/png"
    } else {
        "image/jpeg"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_file_no_folder_is_none() {
        let dir = std::env::temp_dir().join(format!("lyra_art_empty_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("nope.flac");
        // No audio file, no folder image.
        assert_eq!(artwork(&p).unwrap(), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn folder_image_fallback_is_found_case_insensitively() {
        let dir = std::env::temp_dir().join(format!("lyra_art_folder_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A non-parseable "audio" file so the embedded step yields nothing.
        let track = dir.join("song.flac");
        std::fs::write(&track, b"not audio").unwrap();
        std::fs::write(dir.join("Cover.PNG"), b"\x89PNG\r\n\x1a\nfake").unwrap();

        let art = artwork(&track).unwrap().expect("folder image");
        assert_eq!(art.mime, "image/png");
        assert_eq!(&art.data[..4], b"\x89PNG");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn folder_priority_prefers_folder_jpg() {
        let dir = std::env::temp_dir().join(format!("lyra_art_prio_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let track = dir.join("song.flac");
        std::fs::write(&track, b"not audio").unwrap();
        std::fs::write(dir.join("cover.jpg"), b"COVER").unwrap();
        std::fs::write(dir.join("folder.jpg"), b"FOLDER").unwrap();

        let art = artwork(&track).unwrap().expect("folder image");
        assert_eq!(art.data, b"FOLDER");
        std::fs::remove_dir_all(&dir).ok();
    }
}
