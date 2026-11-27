// P8.3b — browser e2e (Playwright) against the Vite dev build with a mocked Tauri
// IPC surface (@tauri-apps/api/mocks). Runs in the installed Edge channel so no
// browser download is needed. This layer gives real CSS layout + a Chromium/Edge
// engine (close to the WebView2 runtime) for the controls whose behaviour depends
// on geometry — chiefly the playlist drag-to-reorder and the app-drawn dropdowns.
import { defineConfig } from "@playwright/test";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const PORT = 5199;
// Vite must run from the repo root so /src/** and /tests/ui/e2e/** resolve.
// Playwright defaults a webServer's cwd to the config directory, which is wrong here.
const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");

export default defineConfig({
  testDir: ".",
  testMatch: /.*\.spec\.ts/,
  fullyParallel: false,
  workers: 1,
  forbidOnly: false,
  retries: 1,
  // The very first spec hits a cold Vite dev server that pre-bundles deps (and may
  // hard-reload once); give it room so cold start does not flake the gate.
  timeout: 60_000,
  reporter: [
    ["list"],
    ["html", { outputFolder: "../../../target/ui/e2e-report", open: "never" }],
  ],
  outputDir: "../../../target/ui/e2e-artifacts",
  use: {
    baseURL: `http://localhost:${PORT}`,
    headless: true,
    channel: "msedge",
    viewport: { width: 1200, height: 700 },
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
  webServer: {
    command: `npx vite --port ${PORT} --strictPort`,
    cwd: REPO_ROOT,
    url: `http://localhost:${PORT}/tests/ui/e2e/index.html`,
    reuseExistingServer: true,
    timeout: 60_000,
  },
});
