// P8.3b UI gate — component layer (lyra-uiqa).
//
// vitest + @testing-library/react in jsdom. Proves every control a user touches
// does its job: Dropdown open/pick/keyboard, Settings persistence, Turntable
// volume knob, the playlist card (select / play / remove / reorder), and that no
// Vietnamese text leaks into the English UI (the 0.4.1 fixes).
//
// Run from the repo root:
//   npx vitest run --config tests/ui/component/vitest.config.ts
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    include: ["tests/ui/component/**/*.test.ts", "tests/ui/component/**/*.test.tsx"],
    setupFiles: ["tests/ui/component/setup.ts"],
    testTimeout: 15000,
  },
});
