// P8.3b e2e — install a mocked Tauri IPC surface BEFORE the app module evaluates.
// This module is imported first by mock-entry.tsx; ES modules evaluate imports in
// order, so window.__TAURI_INTERNALS__ (and the native-backend choice) are in place
// before App.tsx reads its module-load-time `inTauri` const.
//
// Every invoked command and its args are recorded on window.__ipc so the Playwright
// specs can assert exactly which commands ran with which arguments (as the gate
// spec requires). window.__emitEngine pushes engine://event payloads to the app.
import { mockIPC } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";

type Dto = { title: string; artist: string; album: string; length: number; key: string };
const mk = (t: string): Dto => ({ title: t, artist: "", album: "", length: 123, key: t });

const settings = { backend: "native", deviceId: "", mode: "exclusive", dsdMode: "auto" };
try { localStorage.setItem("tt.settings", JSON.stringify(settings)); } catch { /* ignore */ }

let playlists = [
  { id: "mix", title: "Mixtape", isCurrent: true, itemCount: 3 },
  { id: "def", title: "Default Playlist", isCurrent: false, itemCount: 0 },
];
const items: Record<string, Dto[]> = {
  mix: [mk("Alpha Track"), mk("Bravo Song"), mk("Charlie Tune")],
  def: [],
};
const devices = [
  {
    id: "dev-xmos",
    name: "XMOS USB DAC",
    isDefault: true,
    exclusiveFormats: [{ rate: 96000, format: "s24", validBits: 24 }, { rate: 192000, format: "s32", validBits: 24 }],
    dopCapable: true,
    nativeDsdRates: [],
  },
];
let endpointVolume = 0.5;
let cur = { ...settings };

type Call = { cmd: string; args: Record<string, unknown> };
(window as unknown as { __ipc: Call[] }).__ipc = [];

mockIPC((cmd, args: Record<string, unknown> = {}) => {
  // Only record app commands, not the Tauri plugin plumbing.
  if (!cmd.startsWith("plugin:")) (window as unknown as { __ipc: Call[] }).__ipc.push({ cmd, args });
  switch (cmd) {
    case "settings_get": return cur;
    case "settings_set": cur = args.settings as typeof cur; return null;
    case "engine_running": return true;
    case "devices": return devices;
    case "take_pending": return [];
    case "artwork_read": return null;
    case "playlists_list": return playlists.map((p) => ({ ...p, itemCount: items[p.id]?.length ?? 0 }));
    case "playlists_items": return items[args.id as string] ?? [];
    case "playlists_create": {
      const id = "pl" + (playlists.length + 1);
      playlists = [...playlists, { id, title: args.title as string, isCurrent: false, itemCount: 0 }];
      items[id] = [];
      return id;
    }
    case "playlists_reorder": {
      const a = items[args.id as string];
      const [moved] = a.splice(args.from as number, 1);
      a.splice(args.to as number, 0, moved);
      return null;
    }
    case "playlists_remove": {
      const idx = new Set(args.indices as number[]);
      items[args.id as string] = items[args.id as string].filter((_, i) => !idx.has(i));
      return null;
    }
    case "playlists_clear": items[args.id as string] = []; return null;
    case "endpoint_volume_get": return endpointVolume;
    case "endpoint_volume_set": endpointVolume = args.level as number; return null;
    default: return null; // engine_send, engine_shutdown, plugin:* window/webview/dialog/http, ...
  }
}, { shouldMockEvents: true });

// getCurrentWindow()/getCurrentWebview() read __TAURI_INTERNALS__.metadata, which
// mockIPC does not populate. Provide a single "main" window/webview so App.tsx's
// module-load-time getCurrentWindow() call does not throw.
const internals = (window as unknown as { __TAURI_INTERNALS__: Record<string, unknown> }).__TAURI_INTERNALS__;
internals.metadata = {
  currentWindow: { label: "main" },
  currentWebview: { windowLabel: "main", label: "main" },
};

(window as unknown as { __emitEngine: (p: unknown) => void }).__emitEngine = (payload) => {
  void emit("engine://event", payload);
};
