import { hashV1 } from "@ui/shop/seller/lib/entity";
import { z } from "zod";

/** The CIDv1 of each file, keyed by its name: what an item's `contents` names. */
export async function contentHashes(files: readonly File[]): Promise<Record<string, string>> {
  const hashes: Record<string, string> = {};
  for (const file of files) hashes[file.name] = await hashV1(new Uint8Array(await file.arrayBuffer()));
  return hashes;
}

const WEARABLE_METRICS = { meshes: 0, bodies: 0, materials: 0, textures: 0, triangles: 0, entities: 0 };
const EMOTE_METRICS = { sequences: 1, duration: 0, frames: 0, fps: 0 };

/** The metrics upstream's item schema requires before a model is measured. */
export function emptyMetrics(type: "wearable" | "emote"): Record<string, number> {
  return { ...(type === "emote" ? EMOTE_METRICS : WEARABLE_METRICS) };
}

/** A name upstream's item schema takes: 1 to 32 characters, no colon. */
export function validItemName(name: string): boolean {
  const trimmed = name.trim();
  return trimmed.length > 0 && [...trimmed].length <= 32 && !trimmed.includes(":");
}

/** The strings of `value`, when it is a list. */
export const strings = (value: unknown): string[] => Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];

/** An item's `data` as upstream's item schema takes it: a category, every list present and each representation naming its own main file. */
export function itemData(type: "wearable" | "emote", data: Record<string, unknown>): Record<string, unknown> {
  const representations = z.array(z.record(z.string(), z.unknown())).safeParse(data.representations);
  const rows = (representations.success ? representations.data : []).map(rep => {
    const contents = strings(rep.contents).length ? strings(rep.contents) : strings([rep.mainFile]);
    return type === "emote" ? { bodyShapes: rep.bodyShapes, mainFile: rep.mainFile, contents }
      : { ...rep, contents, overrideHides: strings(rep.overrideHides), overrideReplaces: strings(rep.overrideReplaces) };
  });
  const category = typeof data.category === "string" ? data.category : type === "emote" ? "dance" : "upper_body";
  return type === "emote"
    ? { ...data, category, tags: strings(data.tags), representations: rows }
    : { ...data, category, hides: strings(data.hides), replaces: strings(data.replaces), tags: strings(data.tags), representations: rows };
}
