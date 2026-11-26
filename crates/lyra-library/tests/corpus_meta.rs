//! Functional checks of `read_tags`/`artwork` against the real corpus.
//!
//! The corpus at `C:\SoulSeek_Music\complete` (overridable with `LYRA_CORPUS`) is
//! **read-only** test data; this test only reads it. When the corpus is
//! unreachable the test prints a skip note and passes (full parity is proven by
//! `lyra-qa` in P6, which always has the corpus).

use std::path::{Path, PathBuf};

use lyra_library::{artwork, read_tags};

/// Default corpus location; override with the `LYRA_CORPUS` environment variable.
const DEFAULT_CORPUS: &str = r"C:\SoulSeek_Music\complete";

/// Recursively collect up to `limit` files with a `.flac` extension.
fn collect_flacs(root: &Path, limit: usize, out: &mut Vec<PathBuf>) {
    if out.len() >= limit {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= limit {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_flacs(&path, limit, out);
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("flac")) {
            out.push(path);
        }
    }
}

#[test]
fn read_tags_and_artwork_over_corpus() {
    let corpus = std::env::var("LYRA_CORPUS").unwrap_or_else(|_| DEFAULT_CORPUS.to_string());
    let root = Path::new(&corpus);
    if !root.is_dir() {
        eprintln!("skip: corpus not reachable at {corpus}");
        return;
    }

    // Scan the whole corpus so the 2 known MIME-less-picture FLACs are included and
    // their graceful fallback is exercised. Reads only headers/tags, so it is fast.
    let mut files = Vec::new();
    collect_flacs(root, 10_000, &mut files);
    if files.is_empty() {
        eprintln!("skip: no FLAC files found under {corpus}");
        return;
    }

    let mut with_title = 0usize;
    let mut with_art = 0usize;
    for path in &files {
        // read_tags must succeed (never panic) on every real corpus file.
        let meta = read_tags(path)
            .unwrap_or_else(|e| panic!("read_tags failed for {}: {e}", path.display()));
        if meta.title.is_some() {
            with_title += 1;
        }
        // read_tags must expose the FLAC format fields.
        assert!(
            meta.sample_rate.is_some(),
            "no sample rate for {}",
            path.display()
        );

        // artwork must never be an error — a broken embedded picture (the 2 corpus
        // FLACs with a MIME-less picture) must fall back gracefully, not fail.
        let art = artwork(path)
            .unwrap_or_else(|e| panic!("artwork errored for {}: {e}", path.display()));
        if let Some(a) = &art {
            assert!(!a.data.is_empty(), "empty artwork for {}", path.display());
            assert!(!a.mime.is_empty(), "empty mime for {}", path.display());
            with_art += 1;
        }
    }

    eprintln!(
        "corpus meta: {} FLACs read, {} with a title, {} with artwork",
        files.len(),
        with_title,
        with_art
    );
    // At least one real file should carry a title; proves the typed field mapping
    // works end to end against a real container.
    assert!(with_title > 0, "no corpus FLAC exposed a title");
}
