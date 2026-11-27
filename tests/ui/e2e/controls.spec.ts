// P8.3b — browser e2e against the Vite build with mocked Tauri IPC. Verifies the
// controls a user touches and the 0.4.1 fixes in a real browser engine with real
// layout. Each spec records the IPC commands the app issued (window.__ipc) and
// asserts them, and drops a screenshot into target/ui/e2e-screens/.
import { test, expect, type Page } from "@playwright/test";
import { mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const URL = "/tests/ui/e2e/index.html";
const SHOTS = resolve(HERE, "../../../target/ui/e2e-screens");
mkdirSync(SHOTS, { recursive: true });

type Call = { cmd: string; args: Record<string, unknown> };
const ipc = (page: Page) => page.evaluate(() => (window as unknown as { __ipc: Call[] }).__ipc);
const shot = (page: Page, name: string) => page.screenshot({ path: `${SHOTS}/${name}.png` });

async function load(page: Page) {
  await page.goto(URL, { waitUntil: "domcontentloaded" });
  // native backend connects and the Mixtape loads its three tracks. A cold Vite
  // dev server pre-bundles deps on first hit (and may hard-reload once), so allow
  // generous time for the first render.
  await expect(page.locator(".list > li")).toHaveCount(3, { timeout: 20_000 });
}

const VIET = /[À-ÖØ-öø-ɏḀ-ỿ]/;

test.describe("Lyra Turntable UI — browser e2e (mocked IPC)", () => {
  test("loads the app, connects to the native backend, shows the playlist", async ({ page }) => {
    await load(page);
    await expect(page.locator(".status")).toHaveText("Lyra");
    await expect(page.locator(".list > li .t").first()).toHaveText(/Alpha Track/);
    await shot(page, "01-loaded");
  });

  test("no Vietnamese text anywhere (fix #1)", async ({ page }) => {
    await load(page);
    await page.getByRole("button", { name: /File/ }).click();
    await expect(page.getByRole("menu")).toBeVisible();
    const menuText = (await page.getByRole("menu").textContent()) ?? "";
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: "Settings" }).click();
    await expect(page.getByRole("dialog", { name: "Settings" })).toBeVisible();
    const bodyText = (await page.locator("body").textContent()) ?? "";
    const offenders = [...(menuText + bodyText)].filter((ch) => VIET.test(ch));
    expect(offenders, `Vietnamese chars: ${JSON.stringify(offenders)}`).toEqual([]);
    await shot(page, "02-settings-english");
  });

  test("Settings dropdowns open and persist (fix #2)", async ({ page }) => {
    await load(page);
    await page.getByRole("button", { name: "Settings" }).click();
    const dialog = page.getByRole("dialog", { name: "Settings" });
    // Mode dropdown: open, list, pick 'shared'
    const modeField = dialog.locator("fieldset", { hasText: "Mode" });
    await modeField.getByRole("button").click();
    await expect(page.getByRole("listbox")).toBeVisible();
    await expect(page.getByRole("option")).toHaveCount(3);
    await shot(page, "03-mode-dropdown-open");
    await page.getByRole("option", { name: "WASAPI shared (not bit-perfect)" }).click();
    await expect.poll(async () => {
      const calls = await ipc(page);
      return calls.filter((c) => c.cmd === "settings_set").at(-1)?.args.settings;
    }).toMatchObject({ mode: "shared" });
    // DSD dropdown: pick PCM
    const dsdField = dialog.locator("fieldset", { hasText: "DSD" });
    await dsdField.getByRole("button").click();
    await page.getByRole("option", { name: "PCM (convert)" }).click();
    await expect.poll(async () => (await ipc(page)).filter((c) => c.cmd === "settings_set").at(-1)?.args.settings)
      .toMatchObject({ dsdMode: "pcm" });
  });

  test("backend radio switches and persists", async ({ page }) => {
    await load(page);
    await page.getByRole("button", { name: "Settings" }).click();
    // The legacy foobar2000 radio lives inside a collapsed <details> (Advanced /
    // Legacy). Expand it first — a real browser hides collapsed <details> content.
    await page.locator("summary", { hasText: "Advanced / Legacy" }).click();
    await page.getByRole("radio", { name: /foobar2000 \(requires foobar2000\)/ }).check();
    await expect.poll(async () => (await ipc(page)).filter((c) => c.cmd === "settings_set").at(-1)?.args.settings)
      .toMatchObject({ backend: "foobar" });
  });

  test("volume knob drives endpoint volume (fix #3)", async ({ page }) => {
    await load(page);
    const knob = page.getByRole("slider", { name: "Volume" });
    await knob.hover();
    await page.mouse.wheel(0, -120); // wheel up
    await expect.poll(async () => (await ipc(page)).some((c) => c.cmd === "endpoint_volume_set")).toBe(true);
    const sets = (await ipc(page)).filter((c) => c.cmd === "endpoint_volume_set");
    expect(sets.at(-1)!.args.level as number).toBeGreaterThan(0.5);
    await shot(page, "04-volume");
  });

  test("playlist select / Ctrl-click / Shift-click (fix #4)", async ({ page }) => {
    await load(page);
    const rows = page.locator(".list > li");
    await rows.nth(0).locator("button.row").click();
    await expect(rows.nth(0)).toHaveAttribute("data-sel", "true");
    await rows.nth(2).locator("button.row").click({ modifiers: ["Control"] });
    await expect(rows.nth(2)).toHaveAttribute("data-sel", "true");
    await expect(rows.nth(0)).toHaveAttribute("data-sel", "true");
    await rows.nth(0).locator("button.row").click({ modifiers: ["Shift"] });
    for (let i = 0; i < 3; i++) await expect(rows.nth(i)).toHaveAttribute("data-sel", "true");
    await shot(page, "05-selection");
  });

  test("double-click plays a track (engine_send playAt)", async ({ page }) => {
    await load(page);
    await page.locator(".list > li").nth(1).locator("button.row").dblclick();
    await expect.poll(async () =>
      (await ipc(page)).some((c) => c.cmd === "engine_send" && (c.args.cmd as { type?: string })?.type === "playAt"),
    ).toBe(true);
  });

  test("Delete removes the selected track (playlists_remove)", async ({ page }) => {
    await load(page);
    const rows = page.locator(".list > li");
    await rows.nth(1).locator("button.row").click();
    await page.locator(".list").press("Delete");
    await expect(rows).toHaveCount(2);
    expect((await ipc(page)).some((c) => c.cmd === "playlists_remove")).toBe(true);
  });

  test("drag reorders a track (playlists_reorder) — fix #4, real layout", async ({ page }) => {
    await load(page);
    const rows = page.locator(".list > li");
    const b2 = (await rows.nth(2).boundingBox())!;
    const b0 = (await rows.nth(0).boundingBox())!;
    await page.mouse.move(b2.x + b2.width / 2, b2.y + b2.height / 2);
    await page.mouse.down();
    // move up past each row's midpoint in steps so the reorder preview updates
    await page.mouse.move(b0.x + b0.width / 2, b0.y + 4, { steps: 12 });
    await shot(page, "06-dragging");
    await page.mouse.up();
    await expect.poll(async () =>
      (await ipc(page)).find((c) => c.cmd === "playlists_reorder")?.args,
    ).toMatchObject({ from: 2, to: 0 });
  });

  test("transport play button drives the engine", async ({ page }) => {
    await load(page);
    await page.getByRole("button", { name: "Play", exact: true }).click();
    await expect.poll(async () =>
      (await ipc(page)).some((c) => c.cmd === "engine_send" && (c.args.cmd as { type?: string })?.type === "play"),
    ).toBe(true);
  });

  test("sidebar drawer toggles", async ({ page }) => {
    await load(page);
    const drawer = page.locator("button.drawer");
    await expect(drawer).toHaveAttribute("aria-expanded", "true");
    await drawer.click();
    await expect(drawer).toHaveAttribute("aria-expanded", "false");
    await shot(page, "07-sidebar-collapsed");
  });
});
