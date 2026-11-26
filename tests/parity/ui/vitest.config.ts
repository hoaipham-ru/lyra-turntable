// P4.6 UI smoke harness config (lyra-qa, S09).
//
// A device-free, engine-free smoke of the alpha app: mounts <App/> with the Tauri
// surface fully mocked (invoke + engine://event listen), drives the NATIVE backend
// through injected engine events and asserts the UI reflects them. No real engine,
// no audio device, no foobar2000 — safe to run unattended (sound rule N/A).
//
// Run from the repo root:  npx vitest run --config tests/parity/ui/vitest.config.ts
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    include: ["tests/parity/ui/**/*.test.tsx"],
    setupFiles: ["tests/parity/ui/setup.ts"],
    // Keep the suite short and deterministic (ADR-012 fast budget).
    testTimeout: 15000,
  },
});
