//! `lyra-probe dsd` — DSD helpers. Currently `convert` (DFF → DSF fixtures).
//!
//! A thin wrapper over `lyra_dsd`: the conversion logic lives in the crate; this
//! module parses arguments, enforces the ADR-006 target-only write rule and prints
//! a JSON summary. Follows the tool's exit-code contract (0 ok, 3 error).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

use lyra_dsd::probe;

/// Run `dsd convert <input.dff> <output.dsf>`.
pub fn run_convert(input: &Path, output: &Path) -> Result<ExitCode> {
    if !input.exists() {
        bail!("input does not exist: {}", input.display());
    }
    guard_target_only(output)?;
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }

    let before = probe(input).with_context(|| format!("reading {}", input.display()))?;
    lyra_dsd::dff_to_dsf(input, output)
        .with_context(|| format!("converting {} -> {}", input.display(), output.display()))?;
    let after = probe(output).with_context(|| format!("verifying {}", output.display()))?;

    // Sanity: the re-containered file must carry the same DSD rate/channels/length.
    if before.spec.rate != after.spec.rate
        || before.spec.channels != after.spec.channels
        || before.total_frames != after.total_frames
    {
        bail!(
            "conversion mismatch: in {:?}/{}ch/{} frames vs out {:?}/{}ch/{} frames",
            before.spec.rate,
            before.spec.channels,
            before.total_frames,
            after.spec.rate,
            after.spec.channels,
            after.total_frames
        );
    }

    let report = serde_json::json!({
        "op": "dsd.convert",
        "input": input.to_string_lossy(),
        "output": output.to_string_lossy(),
        "rate": after.spec.rate.hz(),
        "channels": after.spec.channels,
        "totalFrames": after.total_frames,
        "outputBytes": std::fs::metadata(output).map(|m| m.len()).unwrap_or(0),
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(ExitCode::SUCCESS)
}

/// Enforce the ADR-006 rule: the tool writes only under a `target/` directory.
fn guard_target_only(output: &Path) -> Result<()> {
    let has_target = output
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case("target"));
    if !has_target {
        bail!(
            "refusing to write outside target/: {} (ADR-006)",
            output.display()
        );
    }
    Ok(())
}

/// Build the default fixture path for a corpus file under `target/corpus-synth/dsf/`.
#[must_use]
pub fn default_fixture_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().unwrap_or_default();
    let mut p = PathBuf::from("target/corpus-synth/dsf");
    let mut name = stem.to_os_string();
    name.push(".dsf");
    p.push(name);
    p
}
