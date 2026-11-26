//! Engine construction options.

use std::path::PathBuf;
use std::time::Duration;

use lyra_core::OutputMode;
use lyra_output::DeviceId;

/// Default gapless pre-roll: open the next decoder this long before the current
/// track ends, so the join has no decode gap (S05, P3.3).
pub(crate) const DEFAULT_PREROLL: Duration = Duration::from_secs(5);

/// How the engine opens its output device and feeds it.
///
/// All fields are plain and public so a backend can build one directly; a
/// [`EngineConfig::default`] targets the system default render endpoint in WASAPI
/// exclusive mode (the bit-perfect default) with no tap.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Which render endpoint to open (`DeviceId::Default` follows the system
    /// default).
    pub device: DeviceId,
    /// The output mode: WASAPI exclusive (default, bit-perfect), WASAPI shared, or
    /// ASIO. The DSD routing (ADR-009) reads this together with the device caps.
    pub mode: OutputMode,
    /// When set, every byte handed to the device is mirrored to this file by
    /// [`lyra_output`]'s off-thread tap (the bit-perfect proof, ADR-004). Used by
    /// `lyra-play --tap` and QA.
    pub tap_path: Option<PathBuf>,
    /// Allow [`lyra_output`]'s timer/poll fill fallback when a driver rejects the
    /// event-driven exclusive path.
    pub allow_poll_fallback: bool,
    /// How long before the current track ends the engine opens the next decoder
    /// (the gapless pre-roll, P3.3). A larger value tolerates slower opens at the
    /// cost of holding the next file open earlier; the default is 5 s.
    pub gapless_preroll: Duration,
    /// When set, the engine opens **no audio device**: every device-format byte is
    /// written to this file instead, consumed instantly. This is the headless,
    /// silent bit-perfect capture path for QA — the file equals what a real device
    /// tap would record. Mutually exclusive with real-device playback; `tap_path`
    /// and `device`/`mode` are ignored while it is set.
    pub offline: Option<PathBuf>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            device: DeviceId::Default,
            mode: OutputMode::WasapiExclusive,
            tap_path: None,
            allow_poll_fallback: false,
            gapless_preroll: DEFAULT_PREROLL,
            offline: None,
        }
    }
}

impl EngineConfig {
    /// A default configuration (system default endpoint, WASAPI exclusive, no tap).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}
