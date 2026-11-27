// Shared jsdom setup for the UI component layer.
//
// 1. Mark the window as "inside Tauri" BEFORE any app module loads so the
//    module-load-time `inTauri` consts (App/Settings/NativeBackend/FoobarBackend)
//    take the native/invoke code paths. setupFiles run before the test imports.
// 2. jsdom lacks the Pointer Capture API that the knob and the playlist
//    drag-to-reorder call; provide no-op stubs so those handlers run.
(globalThis as unknown as { window: Window & { __TAURI_INTERNALS__?: unknown } }).window.__TAURI_INTERNALS__ = {};

// jsdom has no layout, so scrollIntoView is missing; App calls it when the playing
// row changes. A no-op keeps those effects from throwing.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
if (!(Element.prototype as any).scrollIntoView) (Element.prototype as any).scrollIntoView = function () {};

for (const m of ["setPointerCapture", "releasePointerCapture", "hasPointerCapture"] as const) {
  if (!(m in Element.prototype)) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (Element.prototype as any)[m] = function () {
      return false;
    };
  }
}

// jsdom has no SVG layout engine; createSVGPoint / getScreenCTM are used only by
// the needle-drop seek (not by the volume knob), but stub them so a stray call
// does not throw during a Turntable render.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const svgProto = (globalThis as any).SVGSVGElement?.prototype;
if (svgProto && !svgProto.createSVGPoint) {
  svgProto.createSVGPoint = function () {
    return { x: 0, y: 0, matrixTransform: () => ({ x: 0, y: 0 }) };
  };
  svgProto.getScreenCTM = function () {
    return { inverse: () => ({}) };
  };
}
