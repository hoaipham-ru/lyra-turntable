// Single string table for the app's UI strings (polish backlog item 3: keep an
// English/Vietnamese toggle possible). The app UI is Vietnamese today, so `S`
// points at the Vietnamese table; an English mirror lives alongside it so a
// future language switch is a one-line change (set `lang` and re-export `S`).
//
// New UI strings must be added here, not inlined, so the toggle stays feasible.

export type Lang = "vi" | "en";

const vi = {
  // Settings panel. "Cài đặt" = Settings.
  settingsTitle: "Cài đặt",
  settingsBtn: "Cài đặt",
  close: "Đóng",

  // "Bộ máy phát" = Player backend.
  backendLabel: "Bộ máy phát",
  backendFoobar: "foobar2000 (cũ)", // legacy path
  backendNative: "Lyra (bit-perfect)", // the new native engine
  // "Lyra là bộ máy bit-perfect mới; foobar2000 là đường cũ (mặc định tới P8)."
  backendHint: "Lyra là bộ máy bit-perfect mới; foobar2000 là đường cũ, mặc định cho tới P8.",

  // "Thiết bị ra" = Output device.
  deviceLabel: "Thiết bị ra",
  deviceSystemDefault: "Mặc định hệ thống", // System default
  deviceDefaultTag: "(mặc định)", // (default)
  maxRate: "tối đa", // "max" (as in max exclusive rate)

  // "Chế độ" = Mode.
  modeLabel: "Chế độ",
  modeExclusive: "WASAPI độc quyền (bit-perfect)", // exclusive
  modeShared: "WASAPI chia sẻ (không bit-perfect)", // shared, not bit-perfect
  modeAsio: "ASIO",
  // "Chế độ chia sẻ không bit-perfect (Windows có thể trộn/đổi tần số)."
  modeHint: "Chế độ chia sẻ không bit-perfect: Windows có thể trộn và đổi tần số.",

  // DSD routing.
  dsdLabel: "DSD",
  dsdAuto: "Tự động", // auto
  dsdDop: "DoP",
  dsdNative: "Native (ASIO)",
  dsdPcm: "PCM (chuyển đổi)", // convert to PCM
  // "DoP/Native cần DAC hỗ trợ (ADR-009); trên laptop này PCM là đường chạy được."
  dsdHint: "DoP và Native cần DAC hỗ trợ (ADR-009). Trên laptop này, PCM là đường chạy được.",

  // "Sẽ áp dụng khi app chuyển sang máy phát Lyra." Changing the backend only
  // persists the choice now; P4.5 wires it to the running engine.
  applyNote: "Lựa chọn được lưu lại và sẽ áp dụng khi app chuyển sang máy phát Lyra.",

  // Volume knob locked in bit-perfect mode (Turntable).
  // "Bit-perfect: âm lượng khóa ở 0 dB; chỉnh ở DAC/amply."
  volumeLockedTip: "Bit-perfect: âm lượng khóa ở 0 dB; chỉnh ở DAC/amply",

  // Bit-perfect badge.
  badgeTitleBitPerfect: "Bit-perfect: phát nguyên vẹn tới DAC", // bit-perfect passthrough
  badgeTitleProcessed: "Không bit-perfect: tín hiệu bị xử lý", // processed / not bit-perfect

  // P4.5 backend switching.
  engineStarting: "Đang khởi động Lyra…", // native engine warming up (title fallback)
  backendNotReady: "Chưa sẵn sàng bộ máy phát", // the active backend is not connected yet
  savePlaylistTitle: "Lưu playlist", // native m3u export save-dialog title
  playlistSaved: "Đã lưu playlist", // toast after a native m3u export
};

const en: typeof vi = {
  settingsTitle: "Settings",
  settingsBtn: "Settings",
  close: "Close",

  backendLabel: "Player backend",
  backendFoobar: "foobar2000 (legacy)",
  backendNative: "Lyra (bit-perfect)",
  backendHint: "Lyra is the new bit-perfect engine; foobar2000 is the legacy path, default until P8.",

  deviceLabel: "Output device",
  deviceSystemDefault: "System default",
  deviceDefaultTag: "(default)",
  maxRate: "max",

  modeLabel: "Mode",
  modeExclusive: "WASAPI exclusive (bit-perfect)",
  modeShared: "WASAPI shared (not bit-perfect)",
  modeAsio: "ASIO",
  modeHint: "Shared mode is not bit-perfect: Windows may mix and resample.",

  dsdLabel: "DSD",
  dsdAuto: "Auto",
  dsdDop: "DoP",
  dsdNative: "Native (ASIO)",
  dsdPcm: "PCM (convert)",
  dsdHint: "DoP and Native need a capable DAC (ADR-009). On this laptop PCM is the working path.",

  applyNote: "Your choice is saved and takes effect when the app switches to the Lyra backend.",

  volumeLockedTip: "Bit-perfect: volume locked at 0 dB; adjust on the DAC/amp",

  badgeTitleBitPerfect: "Bit-perfect: untouched passthrough to the DAC",
  badgeTitleProcessed: "Not bit-perfect: the signal is processed",

  engineStarting: "Starting Lyra…",
  backendNotReady: "Player backend not ready",
  savePlaylistTitle: "Save playlist",
  playlistSaved: "Playlist saved",
};

const tables = { vi, en };

// Active language. Vietnamese today; flip this (and re-import `S`) for English.
export const lang: Lang = "vi";
export const S = tables[lang];
