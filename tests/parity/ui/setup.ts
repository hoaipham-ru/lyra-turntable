// Mark the jsdom window as "inside Tauri" BEFORE any app module is imported, so the
// module-load-time `inTauri` consts in App.tsx / NativeBackend.ts / FoobarBackend.ts
// / Settings.tsx all see a Tauri context and take the native/invoke code paths.
// setupFiles run before the test module is imported, which is exactly what we need.
(globalThis as unknown as { window: Window & { __TAURI_INTERNALS__?: unknown } }).window.__TAURI_INTERNALS__ = {};
