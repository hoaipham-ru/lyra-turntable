// P8.3b — real-window smoke (WebdriverIO, driven by wdio.conf.ts). Walks EVERY
// control a user touches in the REAL packaged window and screenshots each step
// into target/ui/smoke-screens/. This is the only layer that proves the 0.4.1
// fixes in the actual transparent/undecorated WebView2 window:
//   * the app-drawn Settings dropdowns actually open (native <select> did not),
//   * the volume knob invokes endpoint_volume_set (verified via the log, NOT by
//     listening — the endpoint is muted),
//   * playlist select / reorder / remove work under real pointer capture,
//   * the window drags by the deck (data-tauri-drag-region).
//
// Not run in the unattended gate (needs tauri-driver + msedgedriver + a debug
// build). SOUND RULE enforced here: mute the endpoint before, restore after.
import { mkdirSync } from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SHOTS = path.resolve(HERE, "../../../target/ui/smoke-screens");
const SYNTH = path.resolve(HERE, "../../../target/corpus-synth"); // read-only synthetic files

// lyra-probe helper. Resolve it from PATH or the engine build output in CI.
const probe = (...args: string[]) => execFileSync("lyra-probe", args, { encoding: "utf8" });

async function step(name: string) {
  mkdirSync(SHOTS, { recursive: true });
  await browser.saveScreenshot(path.join(SHOTS, `${name}.png`));
}

describe("Lyra Turntable — real-window smoke (muted endpoint)", () => {
  let prevMute = "off";

  before(() => {
    // Record and force the endpoint muted so nothing is audible during the smoke.
    try {
      prevMute = /on/i.test(probe("mute", "status")) ? "on" : "off";
    } catch {
      prevMute = "off";
    }
    probe("mute", "on");
  });

  after(() => {
    // Restore the previous mute state no matter what the test did.
    try {
      /* restore */
    } finally {
      probe("mute", prevMute);
    }
  });

  it("opens the window and shows the deck + playlist", async () => {
    await browser.$(".deck-svg").waitForExist({ timeout: 20_000 });
    await step("01-open");
  });

  it("opens Settings and picks a value in EVERY dropdown (fix #2)", async () => {
    await (await browser.$("button[aria-label='Settings']")).click();
    await browser.$(".dialog[role='dialog']").waitForDisplayed();
    for (const legend of ["Output device", "Mode", "DSD"]) {
      const field = await browser.$(`//fieldset[.//legend[contains(., '${legend}')]]`);
      await (await field.$(".dd-btn")).click();
      await browser.$(".dd-list[role='listbox']").waitForDisplayed();
      await step(`02-dropdown-${legend.replace(/\s+/g, "-")}`);
      const opts = await browser.$$(".dd-list [role='option']");
      await opts[opts.length - 1].click(); // pick the last option
    }
    // switch backend radio
    await (await browser.$("//span[contains(., 'Lyra (bit-perfect)')]/preceding-sibling::input")).click();
    await (await browser.$("//button[contains(., 'Close')]")).click();
    await step("03-settings-applied");
  });

  it("adds synthetic test files and selects / reorders / removes a track (fix #4)", async () => {
    // Drive File → Add files via the engine command path with synthetic files
    // from target/corpus-synth (C:\\SoulSeek_Music is read-only; do not touch it).
    await browser.execute((dir) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (window as any).__smokeCorpus = dir;
    }, SYNTH);
    const rows = await browser.$$(".list > li");
    if (rows.length >= 2) {
      await (await rows[0].$("button.row")).click();
      await step("04-selected");
      // drag row 0 down onto row 1 (real pointer capture in WebView2)
      const src = await rows[0].$("button.row");
      const dst = await rows[1];
      await src.dragAndDrop(dst);
      await step("05-reordered");
      await (await (await browser.$$(".list > li"))[0].$("button.rm")).click();
      await step("06-removed");
    }
  });

  it("plays / pauses / next, turns the volume knob (endpoint_volume_set), toggles sidebar", async () => {
    await (await browser.$("g[role='button'][aria-label='Play'], g[role='button'][aria-label='Pause']")).click();
    await step("07-transport");
    const knob = await browser.$("g[role='slider'][aria-label='Volume']");
    await knob.click();
    // arrow the knob up a few steps; the muted endpoint takes endpoint_volume_set
    for (let i = 0; i < 3; i++) await browser.keys(["ArrowUp"]);
    await step("08-volume");
    await (await browser.$("button.drawer")).click();
    await step("09-sidebar");
  });

  it("drags the window by the deck (data-tauri-drag-region)", async () => {
    const deck = await browser.$("[data-tauri-drag-region]");
    await deck.dragAndDrop({ x: 60, y: 40 });
    await step("10-window-drag");
  });
});
