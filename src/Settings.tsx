import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { S } from "./strings";

// Settings panel (S08): backend, output device, output mode and DSD routing.
// Persists via the P4.1 Tauri commands `settings_get` / `settings_set`; outside
// Tauri (plain browser / Playwright) it falls back to localStorage so the panel
// still renders and can be driven. P4.5 consumes the persisted backend choice.

type Backend = "foobar" | "native";
type Mode = "exclusive" | "shared" | "asio";
type DsdMode = "auto" | "dop" | "native" | "pcm";

// Mirror of Rust `AppSettings` (serde camelCase) from src-tauri/src/settings.rs.
type AppSettings = { backend: Backend; deviceId: string; mode: Mode; dsdMode: DsdMode };

// Mirror of Rust `DeviceDto` (serde camelCase) from src-tauri/src/engine.rs.
type FormatDto = { rate: number; format: string; validBits: number };
type DeviceDto = {
  id: string;
  name: string;
  isDefault: boolean;
  exclusiveFormats: FormatDto[];
  dopCapable: boolean;
  nativeDsdRates: string[];
};

const inTauri = "__TAURI_INTERNALS__" in window;
const KEY = "tt.settings"; // localStorage fallback key

const DEFAULTS: AppSettings = { backend: "foobar", deviceId: "", mode: "exclusive", dsdMode: "auto" };

function loadLocal(): AppSettings {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) return { ...DEFAULTS, ...JSON.parse(raw) };
  } catch {
    /* storage unavailable */
  }
  return { ...DEFAULTS };
}

// Highest exclusive-mode rate the device accepted on the probe grid, in kHz.
function maxRateKHz(d: DeviceDto): number | null {
  if (!d.exclusiveFormats.length) return null;
  return Math.max(...d.exclusiveFormats.map((f) => f.rate)) / 1000;
}

export function Settings({ onClose }: { onClose: () => void }) {
  const [s, setS] = useState<AppSettings>(DEFAULTS);
  const [devices, setDevices] = useState<DeviceDto[]>([]);
  const dialogRef = useRef<HTMLDivElement>(null);

  // Load current settings + device list on open. Never throws: a missing engine
  // or browser context falls back to localStorage / an empty device list.
  useEffect(() => {
    let alive = true;
    (async () => {
      if (inTauri) {
        try {
          const cur = await invoke<AppSettings>("settings_get");
          if (alive) setS({ ...DEFAULTS, ...cur });
        } catch {
          if (alive) setS(loadLocal());
        }
        try {
          const ds = await invoke<DeviceDto[]>("devices");
          if (alive) setDevices(ds);
        } catch {
          /* engine not running / no audio: keep just the system-default entry */
        }
      } else {
        setS(loadLocal());
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  // Escape to close, and move focus into the dialog on open.
  useEffect(() => {
    const k = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    addEventListener("keydown", k);
    dialogRef.current?.querySelector<HTMLElement>("input,select,button")?.focus();
    return () => removeEventListener("keydown", k);
  }, [onClose]);

  // Persist on every change. settings_set only writes a JSON file, so it is safe
  // even when the engine is not running (the backend swap is P4.5's job).
  const save = (patch: Partial<AppSettings>) => {
    const next = { ...s, ...patch };
    setS(next);
    try {
      localStorage.setItem(KEY, JSON.stringify(next));
    } catch {
      /* storage unavailable */
    }
    if (inTauri) void invoke("settings_set", { settings: next }).catch(() => {});
  };

  return (
    <div className="modal" onClick={onClose}>
      <div
        className="dialog wide"
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={S.settingsTitle}
        onClick={(e) => e.stopPropagation()}
      >
        <h2>{S.settingsTitle}</h2>

        {/* Backend: foobar2000 (legacy) vs Lyra native (bit-perfect). */}
        <fieldset className="sfield">
          <legend>{S.backendLabel}</legend>
          <div className="radios">
            <label>
              <input type="radio" name="backend" checked={s.backend === "foobar"} onChange={() => save({ backend: "foobar" })} />
              <span>{S.backendFoobar}</span>
            </label>
            <label>
              <input type="radio" name="backend" checked={s.backend === "native"} onChange={() => save({ backend: "native" })} />
              <span>{S.backendNative}</span>
            </label>
          </div>
          <p className="hint">{S.backendHint}</p>
        </fieldset>

        {/* Output device: system default + probed render endpoints. */}
        <fieldset className="sfield">
          <legend><label htmlFor="set-device">{S.deviceLabel}</label></legend>
          <select id="set-device" value={s.deviceId} onChange={(e) => save({ deviceId: e.target.value })}>
            <option value="">{S.deviceSystemDefault}</option>
            {devices.map((d) => {
              const khz = maxRateKHz(d);
              const tag = d.isDefault ? ` ${S.deviceDefaultTag}` : "";
              const rate = khz ? ` · ${S.maxRate} ${khz} kHz` : "";
              return (
                <option key={d.id} value={d.id}>
                  {d.name}
                  {tag}
                  {rate}
                </option>
              );
            })}
          </select>
        </fieldset>

        {/* Output mode: exclusive (bit-perfect) / shared / ASIO. */}
        <fieldset className="sfield">
          <legend><label htmlFor="set-mode">{S.modeLabel}</label></legend>
          <select id="set-mode" value={s.mode} onChange={(e) => save({ mode: e.target.value as Mode })}>
            <option value="exclusive">{S.modeExclusive}</option>
            <option value="shared">{S.modeShared}</option>
            <option value="asio">{S.modeAsio}</option>
          </select>
          <p className="hint">{S.modeHint}</p>
        </fieldset>

        {/* DSD routing: auto / DoP / native (ASIO) / PCM. */}
        <fieldset className="sfield">
          <legend><label htmlFor="set-dsd">{S.dsdLabel}</label></legend>
          <select id="set-dsd" value={s.dsdMode} onChange={(e) => save({ dsdMode: e.target.value as DsdMode })}>
            <option value="auto">{S.dsdAuto}</option>
            <option value="dop">{S.dsdDop}</option>
            <option value="native">{S.dsdNative}</option>
            <option value="pcm">{S.dsdPcm}</option>
          </select>
          <p className="hint">{S.dsdHint}</p>
        </fieldset>

        <p className="hint applynote">{S.applyNote}</p>

        <div className="dlgbtns">
          <button type="button" className="primary" onClick={onClose}>{S.close}</button>
        </div>
      </div>
    </div>
  );
}
