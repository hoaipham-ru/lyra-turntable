//! `lyra-probe dsd hash` — QA glue (S09) for DSD byte-equality parity.
//!
//! Decodes a DSF/DFF file to its raw 1-bit DSD byte stream via the public
//! [`lyra_dsd::open`] API and prints a SHA-256 of the stream in two forms:
//! * `sha256Native` — the bytes exactly as the container stores them (DSF is
//!   LSB-first, DFF is MSB-first, see [`lyra_core::DsdSpec::msb_first`]).
//! * `sha256Canonical` — every byte normalised to **MSB-first** so a DFF and its
//!   DSF re-container hash identically (TEST-STRATEGY §4: "DSD bytes identical,
//!   bit-order normalized"). This is the field the parity harness compares.
//!
//! This is read-only QA tooling: it calls the crate's public decoder and never
//! reimplements the DSF/DFF readers or the `dsd convert` path (those are owned by
//! `lyra-dsd`). Exit-code contract: 0 ok, 3 harness/IO error.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use lyra_core::StreamSpec;
use sha2::{Digest, Sha256};

/// Run `dsd hash <file>`; prints a JSON summary with native + canonical hashes.
pub fn run_hash(input: &Path) -> Result<ExitCode> {
    let info = lyra_dsd::probe(input).with_context(|| format!("probing {}", input.display()))?;
    let mut dec = lyra_dsd::open(input).with_context(|| format!("opening {}", input.display()))?;

    let StreamSpec::Dsd(spec) = dec.spec() else {
        anyhow::bail!("{} is not a DSD stream", input.display());
    };

    // Stream the whole file, hashing native bytes and their MSB-first-normalised
    // form incrementally so a 400 MB DFF never has to sit in memory twice.
    let mut native = Sha256::new();
    let mut canonical = Sha256::new();
    let mut total_bytes: u64 = 0;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let frames = dec.read(&mut buf)?;
        if frames == 0 {
            break;
        }
        total_bytes += buf.len() as u64;
        native.update(&buf);
        if spec.msb_first {
            canonical.update(&buf);
        } else {
            // LSB-first container (DSF): reverse each byte to MSB-first.
            for b in &buf {
                canonical.update([b.reverse_bits()]);
            }
        }
    }

    let report = serde_json::json!({
        "op": "dsd.hash",
        "input": input.to_string_lossy(),
        "container": format!("{:?}", info.container),
        "rate": spec.rate.hz(),
        "channels": spec.channels,
        "msbFirst": spec.msb_first,
        "totalFrames": info.total_frames,
        "dsdBytes": total_bytes,
        "sha256Native": hex::encode(native.finalize()),
        "sha256Canonical": hex::encode(canonical.finalize()),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(ExitCode::SUCCESS)
}
