//! `lyra-probe mute on|off|report` — endpoint mute control for the Sound rule.
//!
//! The RUNBOOK Sound rule requires any audio-rendering test to mute the endpoint
//! first and restore the previous mute state afterwards. This uses the Windows
//! **endpoint volume API** (`IAudioEndpointVolume`), which sits *after* the render
//! client in the audio graph and does **not** alter the digital sample stream — so
//! a device tap captured while muted is still byte-for-byte the decoded audio and
//! stays a valid bit-perfect reference (ADR-004 / RUNBOOK).
//!
//! Each call prints JSON. `on`/`off` report `previousMute` so a harness can put the
//! endpoint back exactly as it found it (mute on → run render → mute <previous>).

use anyhow::Result;
use std::process::ExitCode;

/// What to do with the endpoint mute flag.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum MuteAction {
    /// Mute the endpoint (returns the previous state).
    On,
    /// Unmute the endpoint (returns the previous state).
    Off,
    /// Report the current mute state without changing it.
    Report,
}

#[cfg(windows)]
pub fn run(action: MuteAction, device: Option<&str>) -> Result<ExitCode> {
    windows_impl::run(action, device)
}

#[cfg(not(windows))]
pub fn run(_action: MuteAction, _device: Option<&str>) -> Result<ExitCode> {
    anyhow::bail!("`mute` controls a Windows audio endpoint and is only available on Windows")
}

#[cfg(windows)]
mod windows_impl {
    use super::MuteAction;
    use anyhow::{anyhow, Result};
    use std::process::ExitCode;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{BOOL, RPC_E_CHANGED_MODE};
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    /// RAII COM init on this thread; only uninitialises if it did the init.
    struct ComGuard {
        owned: bool,
    }
    impl ComGuard {
        fn enter() -> Result<Self> {
            // SAFETY: standard COM init on the current thread.
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if hr.is_ok() {
                Ok(Self { owned: true })
            } else if hr == RPC_E_CHANGED_MODE {
                // COM already initialised in another apartment; use it, don't uninit.
                Ok(Self { owned: false })
            } else {
                Err(anyhow!("CoInitializeEx failed: {hr:?}"))
            }
        }
    }
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.owned {
                // SAFETY: balances the CoInitializeEx this guard performed.
                unsafe { CoUninitialize() };
            }
        }
    }

    fn wide_nul(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Resolve the target render endpoint (default-console when `device` is None).
    unsafe fn resolve_device(
        enumerator: &IMMDeviceEnumerator,
        device: Option<&str>,
    ) -> Result<IMMDevice> {
        match device {
            None => enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|e| anyhow!("no default render endpoint: {e}")),
            Some(id) => {
                let wide = wide_nul(id);
                enumerator
                    .GetDevice(PCWSTR(wide.as_ptr()))
                    .map_err(|e| anyhow!("no render endpoint with id {id}: {e}"))
            }
        }
    }

    /// The endpoint id string for the JSON report (what callers key on). The
    /// friendly name needs the shell property-store features; the id is enough to
    /// identify the endpoint and keeps the dependency surface small.
    unsafe fn endpoint_id(device: &IMMDevice) -> String {
        device
            .GetId()
            .ok()
            .and_then(|p| if p.is_null() { None } else { p.to_string().ok() })
            .unwrap_or_else(|| "unknown".to_string())
    }

    pub fn run(action: MuteAction, device: Option<&str>) -> Result<ExitCode> {
        let _com = ComGuard::enter()?;
        // SAFETY: all calls are on the thread that initialised COM above; every
        // interface is released on drop at end of scope.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| anyhow!("create MMDeviceEnumerator: {e}"))?;
            let dev = resolve_device(&enumerator, device)?;
            let id = endpoint_id(&dev);

            let endpoint: IAudioEndpointVolume = dev
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| anyhow!("activate IAudioEndpointVolume: {e}"))?;

            let previous = endpoint.GetMute().map_err(|e| anyhow!("GetMute: {e}"))?.as_bool();

            let (want, changed): (Option<bool>, _) = match action {
                MuteAction::On => (Some(true), true),
                MuteAction::Off => (Some(false), true),
                MuteAction::Report => (None, false),
            };

            if let Some(target) = want {
                endpoint
                    .SetMute(BOOL::from(target), std::ptr::null())
                    .map_err(|e| anyhow!("SetMute({target}): {e}"))?;
            }

            let now = endpoint.GetMute().map_err(|e| anyhow!("GetMute: {e}"))?.as_bool();

            let report = serde_json::json!({
                "device": id,
                "id": id,
                "action": match action {
                    MuteAction::On => "on",
                    MuteAction::Off => "off",
                    MuteAction::Report => "report",
                },
                "previousMute": previous,
                "mute": now,
                "changed": changed && previous != now,
            });
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Ok(ExitCode::SUCCESS)
    }
}
