// P8.3b — real-window smoke harness (WebdriverIO + tauri-driver + msedgedriver).
//
// This is the layer that catches WebView2-ONLY bugs (transparent/undecorated
// window, drag regions, pointer capture) — exactly the class of defect that let
// the four 0.4.0 regressions ship. It drives the REAL packaged window, not a
// browser.
//
// It is NOT part of the unattended automated gate, because it needs tooling that
// is not present in this environment and because it opens the real output device:
//   * `cargo install tauri-driver`  (a cargo binary; a human must run it)
//   * msedgedriver.exe matching the installed WebView2/Edge runtime on PATH
//     (download for the local WebView2 version is allowed per the charter)
//   * a DEBUG build of the app:  `npm run tauri build -- --debug`  (or `cargo build`
//     in src-tauri), run from the worktree — never install over Hoai's app.
//
// SOUND RULE: the smoke spec mutes the endpoint (`lyra-probe mute on`) in a
// before() hook and restores the previous mute state in an after() hook's finally.
// foobar2000/Beefweb must not be playing; use synthetic files from
// target/corpus-synth/ (or copies under target/corpus-tmp/).
//
// Run (on a machine with the tooling):  npx wdio run tests/ui/smoke/wdio.conf.ts
import os from "node:os";
import path from "node:path";
import { spawn, type ChildProcess } from "node:child_process";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const APP = path.resolve(HERE, "../../../src-tauri/target/debug/turntable-app.exe");
const TAURI_DRIVER = path.resolve(os.homedir(), ".cargo", "bin", "tauri-driver.exe");

let driver: ChildProcess | undefined;

export const config: WebdriverIO.Config = {
  hostname: "127.0.0.1",
  port: 4444,
  specs: [path.resolve(HERE, "smoke.e2e.ts")],
  maxInstances: 1,
  capabilities: [
    {
      // tauri-driver injects the WebView2 (msedgedriver) session for this binary.
      "tauri:options": { application: APP },
      browserName: "wry",
    } as WebdriverIO.Capabilities,
  ],
  reporters: ["spec"],
  framework: "mocha",
  mochaOpts: { ui: "bdd", timeout: 120_000 },
  logLevel: "warn",

  onPrepare: () => {
    driver = spawn(TAURI_DRIVER, [], { stdio: [null, process.stdout, process.stderr] });
  },
  onComplete: () => {
    driver?.kill();
  },
};
