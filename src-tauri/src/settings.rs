//! App settings: the backend choice and native-engine output options, persisted
//! as a small typed JSON file under the Tauri app config dir.
//!
//! P4.4 builds the settings UI, P4.5 reads the backend choice to pick which
//! `PlayerBackend` the React app talks to. This module only provides the typed
//! storage and the `settings_get` / `settings_set` commands, plus the mapping
//! from stored settings to an [`EngineConfig`] the native engine opens with.

use std::path::PathBuf;

use lyra_core::OutputMode;
use lyra_engine::EngineConfig;
use lyra_output::DeviceId;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// Persisted app settings. Kept small and flat so the JSON file is easy to read
/// and forward-compatible (`#[serde(default)]` fills in any field a future
/// version adds).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    /// Which player drives the UI: `"foobar"` (today's Beefweb path) or
    /// `"native"` (the lyra-engine path). Defaults to `"foobar"`.
    pub backend: String,
    /// Selected render endpoint id (the `IMMDevice::GetId` string from
    /// [`crate::engine::devices`]). Empty string means the system default.
    pub device_id: String,
    /// Output mode: `"exclusive"` (bit-perfect, default), `"shared"`, or `"asio"`.
    pub mode: String,
    /// DSD handling: `"auto"` (default), `"dop"`, `"native"` (ASIO native DSD) or
    /// `"pcm"` (convert to PCM). Only `"native"` currently affects the engine
    /// config (sets `OutputMode::Asio { native_dsd: true }`); the rest are routing
    /// hints the engine resolves against device caps (ADR-009).
    pub dsd_mode: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            backend: "foobar".into(),
            device_id: String::new(),
            mode: "exclusive".into(),
            dsd_mode: "auto".into(),
        }
    }
}

impl AppSettings {
    /// Build the [`EngineConfig`] the native engine should open with. The app
    /// never taps or runs offline, so those fields stay `None`.
    #[must_use]
    pub fn to_engine_config(&self) -> EngineConfig {
        engine_config(&self.device_id, &self.mode, &self.dsd_mode)
    }
}

/// Map the three stored strings to an [`EngineConfig`]. Shared by [`AppSettings`]
/// and `engine_start`'s explicit config so the two never drift.
#[must_use]
pub fn engine_config(device_id: &str, mode: &str, dsd_mode: &str) -> EngineConfig {
    let device = if device_id.is_empty() {
        DeviceId::Default
    } else {
        DeviceId::Endpoint(device_id.to_string())
    };
    let output_mode = match mode {
        "shared" => OutputMode::WasapiShared,
        "asio" => OutputMode::Asio { native_dsd: dsd_mode == "native" },
        _ => OutputMode::WasapiExclusive,
    };
    EngineConfig { device, mode: output_mode, ..EngineConfig::default() }
}

/// Path to the settings file, creating the config dir if needed.
fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("settings.json"))
}

/// Load persisted settings, falling back to defaults when the file is missing or
/// unreadable. Never fails: a broken file yields defaults so the app still starts.
#[must_use]
pub fn load(app: &AppHandle) -> AppSettings {
    settings_path(app)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Read the current app settings.
#[tauri::command]
pub fn settings_get(app: AppHandle) -> AppSettings {
    load(&app)
}

/// Persist app settings to the config dir as JSON.
#[tauri::command]
pub fn settings_set(app: AppHandle, settings: AppSettings) -> Result<(), String> {
    let path = settings_path(&app)?;
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}
