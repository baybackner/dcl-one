import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { beforeAll, expect, it } from "vitest";

import { isRetryable, STATIC_ERROR_STATUSES, statusCopy } from "@ui/components/statusCopy";

const UI3 = fileURLToPath(new URL("../../ui3/", import.meta.url));
const SCRIPT = path.join(UI3, "scripts/gen-error-pages.mts");
const OUT = path.join(UI3, "public/errors");
const run = (...args: string[]) => spawnSync(process.execPath, [SCRIPT, ...args], { cwd: UI3, encoding: "utf8" });
const escape = (text: string) =>
  text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

beforeAll(() => {
  const result = run();
  expect(result.status, result.stderr).toBe(0);
}, 60_000);

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

it("reports the pages up to date right after writing them", () => {
  const result = run("--check");
  expect(result.status, result.stderr).toBe(0);
});
