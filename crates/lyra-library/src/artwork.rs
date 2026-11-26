//! Read-only cover-art lookup (S06, P4.2, P6.2).
//!
//! [`artwork`] returns the best usable cover image for a track:
//!
//! 1. an **embedded** picture read via `lofty` (the file is opened read-only and
//!    never decoded or modified). When a file carries several pictures the
//!    *front cover* is preferred, matching what foobar2000 shows, then an
//!    `Other`/unspecified picture, then any remaining usable picture. Then
//! 2. a **folder image** next to the file — see [`FOLDER_NAMES`] plus
//!    `albumart*.{jpg,jpeg,png,gif}` — in priority order.
//!
//! # Robustness
//! Nothing here is fatal. A file `lofty` cannot parse, or an embedded picture that
//! is broken (two corpus FLACs carry a picture with **no MIME type**), is skipped
//! and the folder fallback is tried instead; if nothing is found the result is
//! simply `Ok(None)`. This mirrors the P1 "broken artwork is skipped with a
//! warning, never fatal" rule.

use std::path::Path;

use lofty::file::TaggedFileExt;
use lofty::picture::{Picture, PictureType};
use lofty::read_from_path;

use lyra_core::Result;

/// A cover image: its MIME type and raw encoded bytes (JPEG/PNG/…).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artwork {
    /// The image MIME type, e.g. `image/jpeg` or `image/png`.
    pub mime: String,
    /// The raw encoded image bytes, exactly as stored.
    pub data: Vec<u8>,
    /// For an embedded picture, its APIC picture type (e.g. `"CoverFront"` or
    /// `"Other"`); `None` for a folder image. Informational only — existing
    /// callers consume [`mime`](Self::mime)/[`data`](Self::data) and ignore this.
    pub picture_type: Option<String>,
}

/// Exact file names searched for a folder image, in priority order. Matched
/// case-insensitively.
///
/// These are the names foobar2000 and common rippers (EAC, dBpoweramp, Windows
/// Media Player) leave beside tracks. In addition to this list, any file named
/// `albumart*.{jpg,jpeg,png,gif}` (WMP's `AlbumArt_*`/`AlbumArtSmall.jpg`) is
/// accepted at lowest priority — see [`folder_artwork`].
const FOLDER_NAMES: &[&str] = &[
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
    "cover.gif",
    "front.jpg",
    "front.jpeg",
    "front.png",
    "album.jpg",
    "album.jpeg",
    "album.png",
];

/// Image extensions accepted for the `albumart*` wildcard match.
const ALBUMART_EXTS: &[&str] = &[".jpg", ".jpeg", ".png", ".gif"];

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

/// Try to read the best usable embedded picture. Returns `None` if the file
/// cannot be parsed or carries no picture with both a MIME type and non-empty
/// data.
fn embedded_artwork(path: &Path) -> Option<Artwork> {
    let tagged = read_from_path(path).ok()?;
    select_picture(tagged.tags().iter().flat_map(|tag| tag.pictures()))
}

/// Pick the best picture from an iterator, preferring the front cover.
///
/// Broken pictures — no MIME type, or empty data — are always skipped so a single
/// bad image never hides a good one. Among usable pictures the order is:
/// `CoverFront`, then `Other`/unspecified, then anything else.
fn select_picture<'a>(pics: impl Iterator<Item = &'a Picture>) -> Option<Artwork> {
    // Lower rank wins. CoverFront is the best possible, so we can stop early.
    let mut best: Option<(u8, Artwork)> = None;
    for pic in pics {
        // A broken embedded picture (no MIME type) or an empty payload is skipped.
        let Some(mime) = pic.mime_type() else {
            continue;
        };
        let data = pic.data();
        if data.is_empty() {
            continue;
        }
        let pic_type = pic.pic_type();
        let rank = match pic_type {
            PictureType::CoverFront => 0u8,
            PictureType::Other => 1,
            _ => 2,
        };
        let improves = best.as_ref().is_none_or(|(r, _)| rank < *r);
        if improves {
            let art = Artwork {
                mime: mime.as_str().to_string(),
                data: data.to_vec(),
                picture_type: Some(format!("{pic_type:?}")),
            };
            if rank == 0 {
                return Some(art);
            }
            best = Some((rank, art));
        }
    }
    best.map(|(_, art)| art)
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
    // 1. Exact, named files in priority order.
    for wanted in FOLDER_NAMES {
        if let Some((lower, p)) = present.iter().find(|(lower, _)| lower == wanted) {
            return read_folder_image(lower, p).map(Some);
        }
    }
    // 2. Fallback: any `albumart*.{jpg,jpeg,png,gif}` (Windows Media Player). Sort
    //    for a deterministic pick when several are present.
    let mut albumart: Vec<&(String, std::path::PathBuf)> = present
        .iter()
        .filter(|(lower, _)| {
            lower.starts_with("albumart") && ALBUMART_EXTS.iter().any(|e| lower.ends_with(e))
        })
        .collect();
    albumart.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some((lower, p)) = albumart.first() {
        return read_folder_image(lower, p).map(Some);
    }
    Ok(None)
}

/// Read a matched folder image and label its MIME type. The MIME is taken from the
/// image's magic bytes when recognisable, falling back to the file extension.
fn read_folder_image(lower_name: &str, p: &Path) -> Result<Artwork> {
    let data = std::fs::read(p).map_err(|e| {
        lyra_core::LyraError::Io(format!("reading cover image {}: {e}", p.display()))
    })?;
    let mime = mime_from_magic(&data)
        .unwrap_or_else(|| mime_from_extension(lower_name))
        .to_string();
    Ok(Artwork {
        mime,
        data,
        picture_type: None,
    })
}

/// Infer a MIME type from an image's leading magic bytes, or `None` if unknown.
fn mime_from_magic(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        Some("image/png")
    } else if data.starts_with(b"GIF8") {
        Some("image/gif")
    } else {
        None
    }
}

/// Guess a MIME type from a lower-cased file name's extension.
fn mime_from_extension(name: &str) -> &'static str {
    if name.ends_with(".png") {
        "image/png"
    } else if name.ends_with(".gif") {
        "image/gif"
    } else {
        "image/jpeg"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lofty::picture::MimeType;

    /// Build an in-memory picture for the selection tests.
    fn pic(pic_type: PictureType, mime: Option<MimeType>, data: &[u8]) -> Picture {
        let mut builder = Picture::unchecked(data.to_vec()).pic_type(pic_type);
        if let Some(m) = mime {
            builder = builder.mime_type(m);
        }
        builder.build()
    }

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
        assert_eq!(art.picture_type, None);
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

    #[test]
    fn albumart_wildcard_is_last_resort() {
        let dir = std::env::temp_dir().join(format!("lyra_art_wmp_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let track = dir.join("song.flac");
        std::fs::write(&track, b"not audio").unwrap();
        // Only a Windows Media Player AlbumArt file is present.
        std::fs::write(dir.join("AlbumArt_{GUID}_Large.jpg"), b"\xFF\xD8\xFFwmp").unwrap();

        let art = artwork(&track).unwrap().expect("albumart image");
        assert_eq!(art.mime, "image/jpeg");
        assert_eq!(&art.data[..3], b"\xFF\xD8\xFF");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn front_cover_is_preferred_over_other_pictures() {
        let other = pic(PictureType::Other, Some(MimeType::Png), b"OTHER");
        let front = pic(PictureType::CoverFront, Some(MimeType::Jpeg), b"FRONT");
        let back = pic(PictureType::CoverBack, Some(MimeType::Jpeg), b"BACK");
        // Front cover wins regardless of order.
        let picked = select_picture([&back, &other, &front].into_iter()).expect("a picture");
        assert_eq!(picked.data, b"FRONT");
        assert_eq!(picked.mime, "image/jpeg");
        assert_eq!(picked.picture_type.as_deref(), Some("CoverFront"));
    }

    #[test]
    fn other_is_preferred_over_non_cover_types() {
        let band = pic(PictureType::Band, Some(MimeType::Jpeg), b"BAND");
        let other = pic(PictureType::Other, Some(MimeType::Png), b"OTHER");
        let picked = select_picture([&band, &other].into_iter()).expect("a picture");
        assert_eq!(picked.data, b"OTHER");
        assert_eq!(picked.picture_type.as_deref(), Some("Other"));
    }

    #[test]
    fn broken_picture_without_mime_is_skipped() {
        // Mirrors the two corpus FLACs: a picture with no MIME type is unusable.
        let broken = pic(PictureType::CoverFront, None, b"BROKEN");
        let good = pic(PictureType::Other, Some(MimeType::Png), b"GOOD");
        // The broken front cover is skipped; the usable Other picture is returned.
        let picked = select_picture([&broken, &good].into_iter()).expect("a picture");
        assert_eq!(picked.data, b"GOOD");
        // With only the broken picture, nothing is returned (falls through to folder).
        assert!(select_picture([&broken].into_iter()).is_none());
    }

    #[test]
    fn empty_payload_picture_is_skipped() {
        let empty = pic(PictureType::CoverFront, Some(MimeType::Jpeg), b"");
        assert!(select_picture([&empty].into_iter()).is_none());
    }

    #[test]
    fn folder_mime_is_inferred_from_magic_bytes() {
        let dir = std::env::temp_dir().join(format!("lyra_art_magic_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let track = dir.join("song.flac");
        std::fs::write(&track, b"not audio").unwrap();
        // A file named .png but whose bytes are actually a JPEG: magic wins.
        std::fs::write(dir.join("cover.png"), b"\xFF\xD8\xFF\xE0jfif").unwrap();

        let art = artwork(&track).unwrap().expect("folder image");
        assert_eq!(art.mime, "image/jpeg");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn folder_mime_falls_back_to_extension_when_bytes_unknown() {
        let dir = std::env::temp_dir().join(format!("lyra_art_ext_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let track = dir.join("song.flac");
        std::fs::write(&track, b"not audio").unwrap();
        // Unrecognisable magic bytes: extension decides the MIME.
        std::fs::write(dir.join("folder.png"), b"xxxxxxxx").unwrap();

        let art = artwork(&track).unwrap().expect("folder image");
        assert_eq!(art.mime, "image/png");
        std::fs::remove_dir_all(&dir).ok();
    }
}
