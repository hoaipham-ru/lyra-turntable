import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { S } from "./strings";
import { Dropdown } from "./Dropdown";

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

// P8.4 cutover: native is the default; foobar2000 is a hidden legacy fallback.
const DEFAULTS: AppSettings = { backend: "native", deviceId: "", mode: "exclusive", dsdMode: "auto" };

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

// `onBackendChange` lets App switch the live backend the moment the user flips the
// radio (P4.5): no app restart needed. A restart still honours the saved choice.
export function Settings({ onClose, onBackendChange }: { onClose: () => void; onBackendChange?: (b: Backend) => void }) {
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
    if (patch.backend) onBackendChange?.(patch.backend);
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

        {/* Backend: Lyra native (bit-perfect) is the default and only normal choice.
            foobar2000 stays available as a hidden legacy fallback in the advanced
            area below (P8.4): Lyra never requires foobar2000. */}
        <fieldset className="sfield">
          <legend>{S.backendLabel}</legend>
          <div className="radios">
            <label>
              <input type="radio" name="backend" checked={s.backend === "native"} onChange={() => save({ backend: "native" })} />
              <span>{S.backendNative}</span>
            </label>
          </div>
          <p className="hint">{S.backendHint}</p>
          <details className="advanced" open={s.backend === "foobar"}>
            <summary>{S.backendLegacyLabel}</summary>
            <div className="radios">
              <label>
                <input type="radio" name="backend" checked={s.backend === "foobar"} onChange={() => save({ backend: "foobar" })} />
                <span>{S.backendFoobar}</span>
              </label>
            </div>
            <p className="hint">{S.backendLegacyHint}</p>
          </details>
        </fieldset>

        {/* Output device: system default + probed render endpoints. */}
        <fieldset className="sfield">
          <legend><label htmlFor="set-device">{S.deviceLabel}</label></legend>
          <Dropdown
            id="set-device"
            value={s.deviceId}
            onChange={(v) => save({ deviceId: v })}
            options={[
              { value: "", label: S.deviceSystemDefault },
              ...devices.map((d) => {
                const khz = maxRateKHz(d);
                const tag = d.isDefault ? ` ${S.deviceDefaultTag}` : "";
                const rate = khz ? ` · ${S.maxRate} ${khz} kHz` : "";
                return { value: d.id, label: d.name + tag + rate };
              }),
            ]}
          />
        </fieldset>

        {/* Output mode: exclusive (bit-perfect) / shared / ASIO. */}
        <fieldset className="sfield">
          <legend><label htmlFor="set-mode">{S.modeLabel}</label></legend>
          <Dropdown<Mode>
            id="set-mode"
            value={s.mode}
            onChange={(v) => save({ mode: v })}
            options={[
              { value: "exclusive", label: S.modeExclusive },
              { value: "shared", label: S.modeShared },
              { value: "asio", label: S.modeAsio },
            ]}
          />
          <p className="hint">{S.modeHint}</p>
        </fieldset>

        {/* DSD routing: auto / DoP / native (ASIO) / PCM. */}
        <fieldset className="sfield">
          <legend><label htmlFor="set-dsd">{S.dsdLabel}</label></legend>
          <Dropdown<DsdMode>
            id="set-dsd"
            value={s.dsdMode}
            onChange={(v) => save({ dsdMode: v })}
            options={[
              { value: "auto", label: S.dsdAuto },
              { value: "dop", label: S.dsdDop },
              { value: "native", label: S.dsdNative },
              { value: "pcm", label: S.dsdPcm },
            ]}
          />
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
