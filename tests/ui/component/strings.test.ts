// P8.3b — string table (src/strings.ts). Fix #1: the shipped UI is English. This
// asserts the active language is English and that no string the app renders from
// the table contains a Vietnamese precomposed Latin letter.
import { describe, it, expect } from "vitest";
import { S, lang } from "../../../src/strings";

// U+00C0..U+1EF9 minus the math symbols × (00D7) and ÷ (00F7).
const VIET = /[À-ÖØ-öø-ɏḀ-ỿ]/;

describe("string table — English UI (fix #1)", () => {
  it("the active language is English", () => {
    expect(lang).toBe("en");
  });

  it("every active UI string is free of Vietnamese letters", () => {
    const offenders = Object.entries(S).filter(([, v]) => typeof v === "string" && VIET.test(v));
    expect(offenders, `Vietnamese strings still active: ${JSON.stringify(offenders)}`).toEqual([]);
  });

  it("key English strings are present", () => {
    expect(S.settingsTitle).toBe("Settings");
    expect(S.modeExclusive).toBe("WASAPI exclusive (bit-perfect)");
    expect(S.dsdPcm).toBe("PCM (convert)");
  });
});
