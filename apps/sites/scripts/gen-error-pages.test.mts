import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, expect, it } from "vitest";

import { isRetryable, STATIC_ERROR_STATUSES, statusCopy } from "@dcl/one-ui/apps/sites/status/statusCopy";

const SITES = fileURLToPath(new URL("../", import.meta.url));
const SCRIPT = path.join(SITES, "scripts/gen-error-pages.mts");
const OUT = fs.mkdtempSync(path.join(os.tmpdir(), "gen-error-pages-"));
const run = (...args: string[]) =>
  spawnSync(process.execPath, [SCRIPT, ...args], { cwd: SITES, env: { ...process.env, ERROR_PAGES_OUT: OUT }, encoding: "utf8" });
const escape = (text: string) =>
  text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

beforeAll(() => {
  const result = run();
  expect(result.status, result.stderr).toBe(0);
}, 60_000);
afterAll(() => fs.rmSync(OUT, { recursive: true, force: true }));

it("writes one page per static status and nothing else", () => {
  expect(fs.readdirSync(OUT).sort()).toEqual(STATIC_ERROR_STATUSES.map((s) => `${s}.html`).sort());
  expect(STATIC_ERROR_STATUSES).toEqual(expect.arrayContaining([404, 429, 500, 503]));
});

it("renders each page with its copy, the home link and the right second action", () => {
  for (const status of STATIC_ERROR_STATUSES) {
    const html = fs.readFileSync(path.join(OUT, `${status}.html`), "utf8");
    const copy = statusCopy(status);
    expect(html, String(status)).toContain(`<title>${status} · ${escape(copy.eyebrow)} · Decentraland</title>`);
    expect(html, String(status)).toContain(escape(copy.title));
    expect(html, String(status)).toContain(escape(copy.message));
    expect(html, String(status)).toMatch(/<a [^>]*href="\/"[^>]*><svg[^>]*>.*?<\/svg><span>Decentraland<\/span><\/a>/s);
    expect(html, String(status)).toMatch(/<a [^>]*href="\/"[^>]*>Go to homepage<\/a>/);
    expect(html, String(status)).toContain(isRetryable(status) ? 'onclick="location.reload()">Try again' : 'onclick="history.back()">Go back');
  }
});

it("inlines the art as real webp bytes, so a Git LFS pointer cannot pass", () => {
  for (const status of STATIC_ERROR_STATUSES) {
    const html = fs.readFileSync(path.join(OUT, `${status}.html`), "utf8");
    const match = /<img [^>]*src="data:image\/webp;base64,([^"]+)"/.exec(html);
    expect(match, String(status)).not.toBeNull();
    const bytes = Buffer.from(match![1]!, "base64");
    expect(bytes.subarray(0, 4).toString("latin1"), String(status)).toBe("RIFF");
    expect(bytes.subarray(8, 12).toString("latin1"), String(status)).toBe("WEBP");
    expect(bytes.length, String(status)).toBeGreaterThan(1024);
  }
});

it("is self-contained: no external stylesheet, script or image, and every custom property it reads is defined", () => {
  for (const status of STATIC_ERROR_STATUSES) {
    const html = fs.readFileSync(path.join(OUT, `${status}.html`), "utf8");
    expect(html, String(status)).not.toMatch(/<link [^>]*rel="stylesheet"|<script[ >]|src="(?!data:)/);
    const defined = new Set([...html.matchAll(/(--[\w-]+)\s*:/g)].map((m) => m[1]));
    const used = [...new Set([...html.matchAll(/var\((--[\w-]+)/g)].map((m) => m[1]))];
    expect(used.filter((name) => !defined.has(name)), String(status)).toEqual([]);
  }
});

it("reports the pages up to date right after writing them, and a stale or unexpected page as out of date", () => {
  expect(run("--check").status).toBe(0);
  const page = path.join(OUT, "503.html");
  const html = fs.readFileSync(page, "utf8");
  fs.writeFileSync(page, html.replace("We'll be right back", "Stale"));
  fs.writeFileSync(path.join(OUT, "999.html"), "");
  try {
    const result = run("--check");
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("503.html, 999.html (unexpected)");
  } finally {
    fs.writeFileSync(page, html);
    fs.rmSync(path.join(OUT, "999.html"));
  }
});
