// Endpoint (device) volume for the native backend.
//
// Uses the Windows endpoint volume API (IAudioEndpointVolume). It sits after the
// render client, so the PCM/DoP bytes Lyra hands to WASAPI are never touched:
// playback stays bit-perfect. On devices with a hardware volume control (USB DACs,
// most onboard codecs) this is the device's own volume; in WASAPI exclusive mode a
// device without hardware volume may ignore it - then adjust on the DAC/amp.

#[cfg(windows)]
mod imp {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};

    struct ComGuard(bool);
    impl ComGuard {
        fn enter() -> Result<Self, String> {
            // SAFETY: standard COM init on the current thread.
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if hr.is_ok() {
                Ok(Self(true))
            } else if hr == RPC_E_CHANGED_MODE {
                Ok(Self(false))
            } else {
                Err(format!("CoInitializeEx failed: {hr:?}"))
            }
        }
    }
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: balances the CoInitializeEx above.
                unsafe { CoUninitialize() };
            }
        }
    }

    fn with_endpoint<T>(device: Option<&str>, f: impl FnOnce(&IAudioEndpointVolume) -> Result<T, String>) -> Result<T, String> {
        let _com = ComGuard::enter()?;
        // SAFETY: COM is initialised on this thread; interfaces are released on drop.
        unsafe {
            let en: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| format!("MMDeviceEnumerator: {e}"))?;
            let dev: IMMDevice = match device.filter(|d| !d.is_empty()) {
                None => en.GetDefaultAudioEndpoint(eRender, eConsole).map_err(|e| format!("default endpoint: {e}"))?,
                Some(id) => {
                    let w: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
                    en.GetDevice(PCWSTR(w.as_ptr())).map_err(|e| format!("endpoint {id}: {e}"))?
                }
            };
            let ep: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None).map_err(|e| format!("IAudioEndpointVolume: {e}"))?;
            f(&ep)
        }
    }

    pub fn get(device: Option<&str>) -> Result<f32, String> {
        // SAFETY: ep is a live interface for the duration of the closure.
        with_endpoint(device, |ep| unsafe { ep.GetMasterVolumeLevelScalar().map_err(|e| format!("get volume: {e}")) })
    }

    pub fn set(device: Option<&str>, level: f32) -> Result<(), String> {
        let level = level.clamp(0.0, 1.0);
        // SAFETY: ep is a live interface for the duration of the closure.
        with_endpoint(device, |ep| unsafe {
            ep.SetMasterVolumeLevelScalar(level, std::ptr::null()).map_err(|e| format!("set volume: {e}"))
        })
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn get(_: Option<&str>) -> Result<f32, String> {
        Err("endpoint volume is Windows-only".into())
    }
    pub fn set(_: Option<&str>, _: f32) -> Result<(), String> {
        Err("endpoint volume is Windows-only".into())
    }
}

/// Current endpoint volume, 0.0..=1.0. `device_id` empty/None = system default.
#[tauri::command]
pub fn endpoint_volume_get(device_id: Option<String>) -> Result<f32, String> {
    imp::get(device_id.as_deref())
}

/// Set the endpoint volume, 0.0..=1.0. Does not touch the audio stream.
#[tauri::command]
pub fn endpoint_volume_set(device_id: Option<String>, level: f32) -> Result<(), String> {
    imp::set(device_id.as_deref(), level)
}
