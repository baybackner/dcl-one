import { strToU8, Zip, ZipPassThrough } from "fflate";

import { catalystBase } from "../client";

type Entity = { title: string; metadataText: string; contents: { file: string; hash: string }[] };

const SCENE_JSON_PATH_RE = /(^|\/)scene\.json$/i;
const CONTENT_FETCH_CONCURRENCY = 4;

function entityFrom(raw: unknown): Entity | null {
  if (!raw || typeof raw !== "object") return null;
  const e = raw as { type?: unknown; pointers?: unknown; content?: unknown; metadata?: unknown };
  if (e.type !== "scene") return null;
  const pointers = Array.isArray(e.pointers) ? e.pointers.filter((p): p is string => typeof p === "string") : [];
  const contents: { file: string; hash: string }[] = [];
  for (const c of Array.isArray(e.content) ? e.content : []) {
    if (!c || typeof c !== "object") continue;
    const file = (c as { file?: unknown; key?: unknown }).file ?? (c as { key?: unknown }).key;
    const hash = (c as { hash?: unknown }).hash;
    if (typeof file === "string" && file && typeof hash === "string" && hash) contents.push({ file, hash });
  }
  const md = e.metadata && typeof e.metadata === "object"
    ? (e.metadata as { display?: { title?: unknown }; scene?: { base?: unknown } })
    : null;
  const title = typeof md?.display?.title === "string" && md.display.title.trim() ? md.display.title.trim() : "Imported scene";
  const base = typeof md?.scene?.base === "string" && md.scene.base ? md.scene.base : pointers[0] ?? "";
  const metadataText = JSON.stringify(e.metadata ?? { scene: { base, parcels: pointers } }, null, 2);
  return { title, metadataText, contents };
}

function slugify(title: string): string {
  return title.toLowerCase().trim().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 64) || "scene";
}

async function fetchContents(contents: Entity["contents"]): Promise<Map<string, Uint8Array>> {
  const base = catalystBase();
  const files = new Map<string, Uint8Array>();
  for (let i = 0; i < contents.length; i += CONTENT_FETCH_CONCURRENCY) {
    await Promise.all(contents.slice(i, i + CONTENT_FETCH_CONCURRENCY).map(async (c) => {
      try {
        const res = await fetch(`${base}/content/contents/${c.hash}`, { credentials: "omit" });
        if (res.ok) files.set(c.file, new Uint8Array(await res.arrayBuffer()));
      } catch {
      }
    }));
  }
  return files;
}

function storedZip(files: Map<string, Uint8Array>): Uint8Array {
  const chunks: Uint8Array[] = [];
  const zip = new Zip((error, chunk) => {
    if (error) throw error;
    chunks.push(chunk);
  });
  for (const [name, data] of files) {
    const entry = new ZipPassThrough(name);
    zip.add(entry);
    entry.push(data, true);
  }
  zip.end();
  const bytes = new Uint8Array(chunks.reduce((size, chunk) => size + chunk.length, 0));
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return bytes;
}

/** The deployed files of scene entity `entityId` as an uncompressed zip named after the scene, or null when it is no scene or holds nothing. */
export async function deployedSceneZip(entityId: string, title?: string): Promise<{ name: string; bytes: Uint8Array } | null> {
  let entity: Entity | null;
  try {
    const res = await fetch(`${catalystBase()}/content/contents/${entityId}`, { headers: { accept: "application/json" } });
    entity = res.ok ? entityFrom(await res.json()) : null;
  } catch {
    entity = null;
  }
  if (!entity) return null;
  const files = await fetchContents(entity.contents);
  if (!entity.contents.some((c) => SCENE_JSON_PATH_RE.test(c.file))) files.set("scene.json", strToU8(entity.metadataText));
  if (files.size === 0) return null;
  return { name: `${slugify(title?.trim() || entity.title)}.zip`, bytes: storedZip(files) };
}
