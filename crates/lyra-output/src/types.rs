//! Cross-platform value types for the output service.

use std::path::PathBuf;

use crate::negotiate::ExclusiveFormat;

/// Which endpoint to open.
///
/// [`DeviceId::Default`] follows the current Windows default render endpoint;
/// [`DeviceId::Endpoint`] pins a specific endpoint by its stable id (the string
/// from `IMMDevice::GetId`, as reported in [`DeviceInfo::id`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DeviceId {
    /// The system default render endpoint.
    #[default]
    Default,
    /// A specific endpoint, identified by its `IMMDevice::GetId` string.
    Endpoint(String),
}

impl DeviceId {
    /// The endpoint id string, or `None` for [`DeviceId::Default`].
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            DeviceId::Default => None,
            DeviceId::Endpoint(s) => Some(s.as_str()),
        }
    }
}

/// A device as seen by enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable WASAPI endpoint id (the string from `IMMDevice::GetId`). Pass this
    /// to [`crate::WasapiDevice::new`] to reopen the same endpoint; `None` there
    /// means the system default render endpoint.
    pub id: String,
    /// Human-readable friendly name.
    pub name: String,
    /// `true` if this is the current default render endpoint.
    pub is_default: bool,
}

/// Options that shape how a stream is opened.
#[derive(Debug, Clone, Default)]
pub struct OutputOptions {
    /// When set, every byte handed to the device is copied to this file from a
    /// non-real-time thread (the device tap, ADR-004 / S04). This is the primary
    /// bit-perfect proof and is what `lyra-play --tap` uses.
    pub tap_path: Option<PathBuf>,
    /// Allow the timer/poll-driven fill fallback when the event handle cannot be
    /// created. The default (event-driven) path is preferred; this only widens
    /// compatibility with drivers that misbehave with event callbacks.
    pub allow_poll_fallback: bool,
}

/// A full capability report for one device, produced by `lyra-play --probe`
/// (ADR-009). The engine (S05) reads this to choose native DSD → DoP → DSD→PCM
/// and to decide which PCM rates can play bit-perfect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProbe {
    /// The device these results belong to.
    pub info: DeviceInfo,
    /// Shared-mode mix format rate reported by the device, if readable.
    pub shared_mix_rate: Option<u32>,
    /// Every `(rate, container, valid-bits)` the device accepted in exclusive
    /// mode across the probed rate × format grid.
    pub exclusive_formats: Vec<ExclusiveFormat>,
    /// Whether this device is marked DoP-capable. Always `false` for an unknown
    /// device — Lyra never sends DoP to a device not known to accept it
    /// (ADR-009). The built-in laptop codec is not DoP-capable.
    pub dop_capable: bool,
    /// Native DSD rates the device accepts over ASIO. Empty without an ASIO
    /// driver (none on the dev laptop); populated in P5.
    pub native_dsd_rates: Vec<lyra_core::DsdRate>,
}

/// The standard PCM sample rates Lyra probes for exclusive support (ADR-004,
/// ADR-010). No resampling is implied — these are only the rates asked about.
pub const PROBE_RATES: [u32; 7] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000, 352_800];
