// P2 render-gate harness (S09, lyra-qa). Obeys the RUNBOOK Sound rule:
// mute the endpoint first, render SHORT clips through lyra-play in WASAPI
// exclusive with the device tap, then RESTORE the previous mute state in a
// finally (even on failure).
//
// For each representative file (ADR-012) x each available endpoint it:
//   1. decodes a slice of the source to raw PCM with lyra-probe (the golden),
//   2. mutes the endpoint (saving previousMute),
//   3. plays the raw through lyra-play --tap (exclusive), capping the run,
//   4. restores the previous mute state,
//   5. strips the tap's prefill/tail silence, and asserts the aligned device
//      bytes are byte-exact to the golden slice (ADR-004 bit-perfect proof).
//
// The tap begins with one prefill-silence buffer and ends with a tail of
// silence once the capped input stops (the feeder zero-fills underruns rather
// than dropping data, crates/lyra-output/src/ring.rs). An INTERIOR zero-frame
// run would be a real steady-state underrun; this harness counts those.
//
// Output: a JSON blob on stdout for the report builder. Nothing is committed
// except this script; taps/raws live under target/corpus-tmp/ (ADR-006).
//
// Usage:
//   node tests/parity/p2_render.mjs --endpoints "<id1>,<id2>" [--max-seconds 10]
//        [--stress-minutes 0] [--stress-endpoint <id>]

import { execFileSync, spawnSync, spawn } from "node:child_process";
import { readFileSync, writeFileSync, appendFileSync, mkdirSync, existsSync } from "node:fs";
import { createHash } from "node:crypto";
import path from "node:path";

const REPO = path.resolve(path.dirname(new URL(import.meta.url).pathname).replace(/^\/([A-Za-z]:)/, "$1"), "..", "..");
const args = parseArgs(process.argv.slice(2));
const MAXSEC = Number(args["max-seconds"] ?? 10);
const STRESS_MIN = Number(args["stress-minutes"] ?? 0);

const PROBE = resolveExe("lyra-probe");
const PLAY = resolveExe("lyra-play");
const TMP = path.join(REPO, "target", "corpus-tmp", "render");
mkdirSync(TMP, { recursive: true });

const manifestPath = args.manifest ?? newestManifest();
const manifest = JSON.parse(readFileSync(manifestPath, "utf8").replace(/^﻿/, ""));

function parseArgs(a) {
  const o = {};
  for (let i = 0; i < a.length; i++) {
    if (a[i].startsWith("--")) { o[a[i].slice(2)] = a[i + 1] && !a[i + 1].startsWith("--") ? a[++i] : true; }
  }
  return o;
}
function resolveExe(name) {
  for (const cfg of ["release", "debug"]) {
    const p = path.join(REPO, "target", cfg, name + ".exe");
    if (existsSync(p)) return p;
  }
  throw new Error(`${name}.exe not found; build it first`);
}
function newestManifest() {
  const dir = path.join(REPO, "tests", "parity", "manifests");
  const files = execFileSync("cmd", ["/c", "dir", "/b", path.join(dir, "golden-*.json")], { encoding: "utf8" })
    .trim().split(/\r?\n/).filter(Boolean).sort().reverse();
  return path.join(dir, files[0]);
}
function srcFmt(bits) { return bits <= 16 ? { fmt: "s16", bits: 16 } : bits <= 24 ? { fmt: "s24", bits: 24 } : { fmt: "s32", bits: 32 }; }
function bpf(bits, ch) { return (bits <= 16 ? 2 : bits <= 24 ? 3 : 4) * ch; }
function sha(buf) { return createHash("sha256").update(buf).digest("hex"); }

// Decode a source slice to raw PCM (the golden, packed container).
function decodeSlice(file, frames, out) {
  const r = spawnSync(PROBE, ["decode", file, "--engine", "lyra", "--seek", "0", "--frames", String(frames), "--out", out, "--format", "raw"], { encoding: "utf8", maxBuffer: 1 << 28 });
  if (r.status !== 0) throw new Error(`decode failed: ${r.stderr}`);
  return JSON.parse(r.stdout);
}
function mute(action, device) {
  const r = spawnSync(PROBE, ["mute", action, "--device", device], { encoding: "utf8" });
  if (r.status !== 0) throw new Error(`mute ${action} failed: ${r.stderr}`);
  return JSON.parse(r.stdout);
}
function play(rawIn, device, rate, ch, s, tap, maxSec) {
  const r = spawnSync(PLAY, [rawIn, "--device", device, "--rate", String(rate), "--channels", String(ch), "--bits", String(s.bits), "--format", s.fmt, "--tap", tap, "--max-seconds", String(maxSec)], { encoding: "utf8", maxBuffer: 1 << 26 });
  return { status: r.status, stdout: r.stdout || "", stderr: r.stderr || "" };
}
function parsePlay(stdout) {
  const neg = /negotiated:\s+(\d+)\s*Hz,\s*(\d+)\s*ch,\s*(\w+)\s*container\s*\[([^\]]+)\]/.exec(stdout);
  const done = /underruns:\s*(\d+),\s*device lost:\s*(\w+)/.exec(stdout);
  const drop = /Tap dropped bytes:\s*(\d+)/.exec(stdout);
  return {
    negRate: neg ? +neg[1] : null, negCh: neg ? +neg[2] : null, negContainer: neg ? neg[3] : null,
    bitPerfect: neg ? /bit-perfect/.test(neg[4]) && !/NOT/.test(neg[4]) : false,
    underruns: done ? +done[1] : null, deviceLost: done ? done[2] === "true" : null,
    tapDropped: drop ? +drop[1] : null,
  };
}
// Aligned bit-perfect comparison of a device tap against the golden slice.
//
// The tap = [prefill silence] + [exact copy of golden, in order] + [silence tail
// once the capped input stops]. The feeder zero-fills an underrun instead of
// dropping data, so an INTERIOR underrun would appear as injected silence that is
// NOT in the golden -> a mismatch before the tail. We align by the prefill
// (tapLeadingZeros - goldenLeadingZeros, which also respects the golden's own
// leading digital silence) and then compare frame by frame.
//   verdict BITEXACT_FULL_OVERLAP     - every overlapping frame equal
//   verdict BITEXACT_THEN_SILENCE_TAIL- equal until the capped render stops, then
//                                       the tap is pure silence (expected)
//   verdict MISMATCH                  - a non-silence difference = real corruption
function alignedCompare(T, G, bpf) {
  const nT = (T.length / bpf) | 0, nG = (G.length / bpf) | 0;
  const zT = (i) => { for (let b = i * bpf; b < i * bpf + bpf; b++) if (T[b]) return false; return true; };
  const zG = (i) => { for (let b = i * bpf; b < i * bpf + bpf; b++) if (G[b]) return false; return true; };
  let tapLead = 0; while (tapLead < nT && zT(tapLead)) tapLead++;
  let goldLead = 0; while (goldLead < nG && zG(goldLead)) goldLead++;
  const P = tapLead - goldLead;
  let tapTrail = 0; while (tapTrail < nT && zT(nT - 1 - tapTrail)) tapTrail++;
  const L = Math.min(nT - P, nG);
  let firstDiff = -1;
  for (let i = 0; i < L; i++) {
    const to = (P + i) * bpf, go = i * bpf; let d = false;
    for (let b = 0; b < bpf; b++) if (T[to + b] !== G[go + b]) { d = true; break; }
    if (d) { firstDiff = i; break; }
  }
  let verdict, rendered = L, tailOnly = false;
  if (firstDiff === -1) verdict = "BITEXACT_FULL_OVERLAP";
  else {
    tailOnly = true;
    for (let i = firstDiff; i < nT - P; i++) if (!zT(P + i)) { tailOnly = false; break; }
    rendered = firstDiff;
    verdict = tailOnly ? "BITEXACT_THEN_SILENCE_TAIL" : "MISMATCH";
  }
  return { nT, nG, tapLead, goldLead, prefillP: P, tapTrail, overlapFrames: L, firstDiff, renderedFrames: rendered, tailOnly, verdict };
}

const results = { generated: new Date().toISOString(), manifest: path.basename(manifestPath), corpusHash: manifest.corpusHash, maxSeconds: MAXSEC, taps: [], stress: null };

// Build the representative set.
const flac = manifest.files.filter((f) => f.ext === "flac");
const byRateBits = {};
for (const f of flac) { const k = `${f.sampleRate}/${f.bits}`; if (!byRateBits[k]) byRateBits[k] = f; }
const reps = [];
for (const k of ["44100/16", "44100/24", "48000/24", "96000/24", "192000/24"]) if (byRateBits[k]) reps.push({ tag: k, ...byRateBits[k] });
for (const f of flac) if (/Qing Hua Ci|Lan Ting Xu/.test(f.path)) reps.push({ tag: "broken-pic " + f.sampleRate + "/" + f.bits, ...f });
const wav = manifest.files.find((f) => f.ext === "wav"); if (wav) reps.push({ tag: "wav " + wav.sampleRate + "/" + wav.bits, ...wav });

const endpoints = String(args.endpoints || "").split(",").map((s) => s.trim()).filter(Boolean);

for (const ep of endpoints) {
  for (const f of reps) {
    const s = srcFmt(f.bits);
    const fb = bpf(f.bits, f.channels);
    const safe = f.tag.replace(/[^A-Za-z0-9]/g, "_");
    const raw = path.join(TMP, `src_${safe}.raw`);
    const tap = path.join(TMP, `tap_${safe}.tap`);
    const row = { endpoint: ep, tag: f.tag, file: path.basename(f.path), spec: `${f.sampleRate}/${f.bits}/${f.channels}ch` };
    let prev = null;
    try {
      decodeSlice(f.path, f.sampleRate * (MAXSEC + 2), raw);
      const golden = readFileSync(raw);
      const m = mute("on", ep); prev = m.previousMute;
      const p = play(raw, ep, f.sampleRate, f.channels, s, tap, MAXSEC);
      const pp = parsePlay(p.stdout);
      Object.assign(row, pp, { playStatus: p.status });
      if (p.status !== 0 && /DEVICE_IN_USE|in use|busy/i.test(p.stderr)) { row.result = "DEVICE_BUSY"; row.note = p.stderr.trim().slice(0, 160); }
      else if (p.status !== 0) { row.result = "PLAY_FAIL"; row.note = p.stderr.trim().slice(0, 200); }
      else {
        const tapBuf = readFileSync(tap);
        const c = alignedCompare(tapBuf, golden, fb);
        Object.assign(row, c);
        const clean = (c.verdict === "BITEXACT_FULL_OVERLAP" || c.verdict === "BITEXACT_THEN_SILENCE_TAIL");
        row.result = clean && pp.tapDropped === 0 ? "PASS" : "FAIL";
        if (c.verdict === "MISMATCH") row.firstDiffFrame = c.firstDiff;
      }
    } catch (e) {
      row.result = "ERROR"; row.note = String(e.message || e).slice(0, 200);
    } finally {
      if (prev !== null) { try { mute(prev ? "on" : "off", ep); } catch {} }
    }
    results.taps.push(row);
    process.stderr.write(`[tap] ${ep.slice(-8)} ${f.tag} -> ${row.result}\n`);
  }
}

// --- stress: long render under CPU load on one endpoint, count interior underruns.
if (STRESS_MIN > 0) {
  const ep = args["stress-endpoint"] || endpoints[0];
  const base = byRateBits["48000/24"] || reps[0];
  const s = srcFmt(base.bits), fb = bpf(base.bits, base.channels);
  const unit = path.join(TMP, "stress_unit.raw");
  const big = path.join(TMP, "stress_big.raw");
  const tap = path.join(TMP, "stress.tap");
  decodeSlice(base.path, base.sampleRate * 60, unit); // ~60 s unit
  const unitBuf = readFileSync(unit);
  const need = Math.ceil((STRESS_MIN * 60 + 10) / 60);
  writeFileSync(big, Buffer.alloc(0));
  for (let i = 0; i < need; i++) appendFileSync(big, unitBuf);
  // background CPU load: spin N workers
  const nproc = Number(execFileSync("cmd", ["/c", "echo", "%NUMBER_OF_PROCESSORS%"], { encoding: "utf8" }).trim()) || 4;
  const loaders = [];
  for (let i = 0; i < Math.max(2, nproc - 1); i++) {
    loaders.push(spawnSyncBg());
  }
  let prev = null; let stressRow = { endpoint: ep, minutes: STRESS_MIN, loaders: loaders.length };
  try {
    const m = mute("on", ep); prev = m.previousMute;
    const t0 = Date.now();
    const p = play(big, ep, base.sampleRate, base.channels, s, tap, STRESS_MIN * 60);
    const wall = (Date.now() - t0) / 1000;
    const pp = parsePlay(p.stdout);
    const tapBuf = readFileSync(tap);
    const bigBuf = readFileSync(big);
    const c = alignedCompare(tapBuf, bigBuf, fb);
    Object.assign(stressRow, pp, { wallSeconds: Math.round(wall), ...c });
    const clean = (c.verdict === "BITEXACT_FULL_OVERLAP" || c.verdict === "BITEXACT_THEN_SILENCE_TAIL");
    stressRow.result = clean && pp.tapDropped === 0 ? "PASS" : "FAIL";
  } catch (e) { stressRow.result = "ERROR"; stressRow.note = String(e.message || e); }
  finally { if (prev !== null) { try { mute(prev ? "on" : "off", ep); } catch {} } for (const l of loaders) { try { process.kill(l.pid); } catch {} } }
  results.stress = stressRow;
}

function spawnSyncBg() {
  // a cheap busy-loop process to load the CPU during the stress render
  return spawn(process.execPath, ["-e", "const e=Date.now()+7200000;while(Date.now()<e){Math.sqrt(Math.random()*1e9);}"], { detached: true, stdio: "ignore" });
}

writeFileSync(path.join(TMP, "p2_results.json"), JSON.stringify(results, null, 2));
process.stdout.write(JSON.stringify(results));
