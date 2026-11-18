// Builds public/errors/<status>.html: self-contained branded error pages that nginx serves for errors
// it generates itself (the app is down or timed out, rate limits, `return 404`, oversized bodies). The
// app's own 4xx/5xx render StatusPage (library/ui/apps/sites/status), whose copy, CSS Module and art
// these pages share.
//
// Every page inlines its CSS, the --dcl-* tokens that CSS reads (from library/ui/foundations), the logo
// and the art (as a data: URI), so it renders even when nothing behind nginx is reachable.
//
// The output is not committed: scripts/sync-ui3-public.mts runs this after it fills public/ (predev,
// prebuild, the Nix sites package), so every build renders the pages from the current sources.
//
//   node scripts/gen-error-pages.mts [--check]
//   ERROR_PAGES_OUT=<dir> writes elsewhere than public/errors.
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";

const sites = fileURLToPath(new URL("../", import.meta.url));
const oneUi = fileURLToPath(new URL("../../../library/ui/", import.meta.url));
const status = join(oneUi, "apps/sites/status");
const artDir = join(oneUi, "apps/sites/assets/errors");
const foundations = join(oneUi, "foundations");
const outDir = process.env.ERROR_PAGES_OUT || join(sites, "public/errors");
const check = process.argv.includes("--check");

// The sources are TypeScript, a CSS Module and TSX: Vite (which every Sites install carries) loads
// them, so the class names and the compiled CSS are the ones the module produces.
const vite = await createServer({
  configFile: false,
  root: sites,
  logLevel: "silent",
  appType: "custom",
  server: { middlewareMode: true, hmr: false, watch: null },
  resolve: { dedupe: ["react", "react-dom"] },
  optimizeDeps: { noDiscovery: true, include: [] },
});
let copyModule: typeof import("../../../library/ui/apps/sites/status/statusCopy.ts");
let styles: Record<string, string>;
let moduleCss: string;
let icons: typeof import("../../../library/ui/foundations/icons/index.ts");
try {
  copyModule = await vite.ssrLoadModule(join(status, "statusCopy.ts")) as typeof copyModule;
  styles = (await vite.ssrLoadModule(join(status, "StatusPage.module.css"))).default as Record<string, string>;
  moduleCss = (await vite.ssrLoadModule(`${join(status, "StatusPage.module.css")}?inline`)).default as string;
  icons = await vite.ssrLoadModule(join(foundations, "icons/index.ts")) as typeof icons;
} finally {
  await vite.close();
}
const { ART_FILES, LOGOMARK_VIEW_BOX, STATIC_ERROR_STATUSES, isRetryable, statusCopy } = copyModule;

const MEDIA_BLOCK = /@media[^{]*\{(?:[^{}]*\{[^{}]*\})*\s*\}/g;

/** The --dcl-* values the foundation stylesheets declare outside media queries, and the names a media query overrides. */
function foundationTokens(): { values: Map<string, string>; responsive: Set<string> } {
  const values = new Map<string, string>();
  const responsive = new Set<string>();
  const sheets = (dir: string): string[] =>
    readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
      e.isDirectory() ? sheets(join(dir, e.name)) : e.name.endsWith(".css") && !e.name.endsWith(".module.css") ? [join(dir, e.name)] : [],
    );
  for (const file of sheets(foundations)) {
    const css = readFileSync(file, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    for (const block of css.matchAll(MEDIA_BLOCK)) {
      for (const m of block[0].matchAll(/(--dcl-[\w-]+)\s*:/g)) responsive.add(m[1]!);
    }
    for (const m of css.replace(MEDIA_BLOCK, "").matchAll(/(--dcl-[\w-]+)\s*:\s*([^;]+);/g)) {
      const name = m[1]!;
      const value = m[2]!.trim();
      if (values.has(name) && values.get(name) !== value) throw new Error(`gen-error-pages: ${name} has two values in library/ui/foundations`);
      values.set(name, value);
    }
  }
  return { values, responsive };
}

/** `:root{...}` with every token the CSS reads, and the tokens those read, sorted by name. */
function tokenBlock(css: string): string {
  const { values, responsive } = foundationTokens();
  const needed = new Map<string, string>();
  const visit = (text: string) => {
    for (const m of text.matchAll(/var\(\s*(--dcl-[\w-]+)/g)) {
      const name = m[1]!;
      if (needed.has(name)) continue;
      const value = values.get(name);
      if (value === undefined) throw new Error(`gen-error-pages: ${name} is not defined in library/ui/foundations`);
      if (responsive.has(name)) throw new Error(`gen-error-pages: ${name} changes per breakpoint; the static pages carry no media queries`);
      needed.set(name, value);
      visit(value);
    }
  };
  visit(css);
  return `:root{${[...needed].sort(([a], [b]) => a.localeCompare(b)).map(([name, value]) => `${name}:${value}`).join(";")}}`;
}

const escape = (text: string) =>
  text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const cls = (...names: string[]) =>
  names.map((name) => {
    const scoped = styles[name];
    if (!scoped) throw new Error(`gen-error-pages: StatusPage.module.css has no .${name}`);
    return scoped;
  }).join(" ");

const LFS_POINTER = "version https://git-lfs.github.com/spec/v1";
const artData = new Map<string, string>();
const artUri = (file: string) => {
  if (!artData.has(file)) {
    const bytes = readFileSync(join(artDir, file));
    if (bytes.subarray(0, LFS_POINTER.length).toString("latin1") === LFS_POINTER) {
      throw new Error(`gen-error-pages: ${file} is a Git LFS pointer. Run git lfs pull.`);
    }
    if (bytes.subarray(0, 4).toString("latin1") !== "RIFF" || bytes.subarray(8, 12).toString("latin1") !== "WEBP") {
      throw new Error(`gen-error-pages: ${file} is not a webp image`);
    }
    artData.set(file, `data:image/webp;base64,${bytes.toString("base64")}`);
  }
  return artData.get(file)!;
};

const css = moduleCss.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\s*\n\s*/g, "");
const style = `${tokenBlock(css)}html,body{margin:0;background:#0d0c11}${css}`;
const logo = renderToStaticMarkup(
  createElement(icons.GeneralDecentraland, { className: styles.logoMark, size: 28, viewBox: LOGOMARK_VIEW_BOX, "aria-hidden": "true" }),
);

function page(code: number): string {
  const copy = statusCopy(code);
  const art = ART_FILES[copy.art];
  const second = isRetryable(code)
    ? `<button type="button" class="${cls("button", "secondary")}" onclick="location.reload()">Try again</button>`
    : `<button type="button" class="${cls("button", "secondary")}" onclick="history.back()">Go back</button>`;
  return `<!doctype html>
<!-- Generated by apps/sites/scripts/gen-error-pages.mts from library/ui/apps/sites/status. Do not edit. -->
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>${code} · ${escape(copy.eyebrow)} · Decentraland</title>
<style>${style}</style>
</head>
<body>
<main class="${cls("page")}" aria-labelledby="sp-title" data-status="${code}">
<div class="${cls("card")}">
<a class="${cls("logo")}" href="/">${logo}<span>Decentraland</span></a>
<img class="${cls("art")}" src="${artUri(art.file)}" width="${art.width}" height="${art.height}" alt="">
<p class="${cls("code")}">${code} · ${escape(copy.eyebrow)}</p>
<h1 id="sp-title" class="${cls("title")}">${escape(copy.title)}</h1>
<p class="${cls("message")}">${escape(copy.message)}</p>
<nav class="${cls("actions")}" aria-label="Error page navigation"><a class="${cls("button", "primary")}" href="/">Go to homepage</a>${second}</nav>
</div>
</main>
</body>
</html>
`;
}

mkdirSync(outDir, { recursive: true });
const expected = new Map(STATIC_ERROR_STATUSES.map((code) => [`${code}.html`, page(code)]));
const stale: string[] = [];
for (const [name, html] of expected) {
  const file = join(outDir, name);
  let current = "";
  try { current = readFileSync(file, "utf8"); } catch {}
  if (current === html) continue;
  if (check) stale.push(name);
  else writeFileSync(file, html);
}
for (const name of readdirSync(outDir)) {
  if (!expected.has(name)) stale.push(`${name} (unexpected)`);
}
if (check && stale.length) {
  console.error(`[gen:error-pages] out of date: ${stale.join(", ")}\n  run: node scripts/gen-error-pages.mts`);
  process.exit(1);
}
console.log(check ? "[gen:error-pages] up to date" : `[gen:error-pages] wrote ${expected.size} pages to ${outDir}`);
