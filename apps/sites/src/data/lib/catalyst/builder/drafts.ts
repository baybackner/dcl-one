import { validateModelFile } from "./model-file";
import { z } from "zod";
import { formatUnits, parseUnits } from "viem";
import { catalystBase, CatalystError } from "../client";
import { contentHashes, emptyMetrics, itemData, strings, validItemName } from "./item-files";

export type WearableDraft = {
  collectionId: string;
  itemId: string;
  name: string;
  modelFile: string;
  model?: File;
  category: string;
  rarity: string;
  price: string;
  free: boolean;
};

export type DraftOptions = {
  fetch: (url: string, init?: RequestInit) => Promise<Response>;
  base?: string;
  signal?: AbortSignal;
  /** The signing wallet: what a save names as the item's `eth_address`. */
  address?: string;
};

const Item = z.object({
  id: z.string().uuid(), name: z.string(), type: z.enum(["wearable", "emote"]),
  collection_id: z.string().nullable(), description: z.string().nullish().transform(value => value ?? ""), thumbnail: z.string().nullable(),
  eth_address: z.string().optional(), metrics: z.record(z.string(), z.number()).nullish(),
  rarity: z.string().nullable(), price: z.string().nullable(), beneficiary: z.string().nullable(),
  is_published: z.boolean(), data: z.record(z.string(), z.unknown()),
  contents: z.record(z.string(), z.string()), urn: z.string().nullable().optional(),
});

export async function draftRequest(path: string, opts: DraftOptions, init: RequestInit = {}): Promise<unknown> {
  const url = `${catalystBase(opts.base)}${path}`;
  const response = await opts.fetch(url, { ...init, signal: opts.signal, cache: "no-store" });
  const body = await response.json().catch(() => null) as { data?: unknown; message?: string; error?: string } | null;
  if (!response.ok || (body as { ok?: boolean } | null)?.ok === false) throw new CatalystError(body?.message ?? body?.error ?? `Could not save the draft (${response.status}).`, url, response.status);
  if (!body || (!("data" in body) && (body as { ok?: boolean }).ok !== true)) throw new Error("Builder returned an invalid draft response.");
  return body.data;
}

/** `POST /v1/items/:id/files` after the item names the files: each file sent under the content hash the item names for it, once per hash. */
export async function uploadItemFiles(path: string, opts: DraftOptions, files: readonly File[], hashes: Record<string, string>): Promise<void> {
  const form = new FormData();
  const sent = new Set<string>();
  for (const file of files) {
    const hash = hashes[file.name];
    if (!hash || sent.has(hash)) continue;
    sent.add(hash);
    form.append(hash, file, file.name);
  }
  if (sent.size) await draftRequest(`${path}/files`, opts, { method: "POST", body: form });
}

function itemPath(id: string): string {
  if (!z.string().uuid().safeParse(id).success) throw new Error("Invalid wearable draft id.");
  return `/v1/items/${id}`;
}

export async function loadWearableDraft(id: string, opts: DraftOptions): Promise<WearableDraft> {
  const item = Item.parse(await draftRequest(itemPath(id), opts));
  if (item.type !== "wearable") throw new Error("Open emotes in the emote editor.");
  if (item.is_published) throw new Error("Published wearables cannot be edited as drafts.");
  const reps = z.array(z.object({ mainFile: z.string() })).safeParse(item.data.representations);
  return {
    itemId: item.id, collectionId: item.collection_id ?? "", name: item.name,
    modelFile: reps.success ? reps.data[0]?.mainFile ?? "" : "",
    category: typeof item.data.category === "string" ? item.data.category : "upper_body",
    rarity: item.rarity ?? "common", price: formatUnits(BigInt(item.price ?? "0"), 18),
    free: BigInt(item.price ?? "0") === 0n,
  };
}

const BOTH_BODY_SHAPES = ["urn:decentraland:off-chain:base-avatars:BaseMale", "urn:decentraland:off-chain:base-avatars:BaseFemale"];
export async function saveWearableDraft(draft: WearableDraft, opts: DraftOptions): Promise<{ itemId: string; urn: string }> {
  const path = itemPath(draft.itemId);
  if (!validItemName(draft.name)) throw new Error("Enter a wearable name of 1 to 32 characters without a colon.");
  if (!draft.free && !/^\d+(\.\d{1,18})?$/.test(draft.price)) throw new Error("Enter a non-negative MANA price with at most 18 decimal places.");
  const price = draft.free ? "0" : parseUnits(draft.price, 18).toString();
  const model = draft.model;
  if (model) await validateModelFile(model);
  let existing: z.infer<typeof Item> | undefined;
  try { existing = Item.parse(await draftRequest(path, opts)); }
  catch (error) { if (!(error instanceof CatalystError && error.status === 404)) throw error; }
  if (existing?.is_published) throw new Error("Published wearables cannot be edited as drafts.");
  if (existing && existing.type !== "wearable") throw new Error("Open emotes in the emote editor.");
  if (!model && !existing?.contents[draft.modelFile]) throw new Error("Choose a model before saving this wearable.");
  const contents: Record<string, string> = { ...existing?.contents };
  const data: Record<string, unknown> = { ...existing?.data, category: draft.category };
  const hashes = model ? await contentHashes([model]) : {};
  if (model) {
    Object.assign(contents, hashes);
    const representations = z.array(z.record(z.string(), z.unknown())).safeParse(existing?.data.representations);
    const previous = representations.success ? representations.data : [];
    const mainFile = previous[0]?.mainFile;
    data.representations = previous.length ? previous.map(rep => rep.mainFile === mainFile ? {
      ...rep, mainFile: model.name,
      contents: [...strings(rep.contents).filter(file => file !== mainFile && file !== model.name), model.name],
    } : rep) : [{ bodyShapes: BOTH_BODY_SHAPES, mainFile: model.name, contents: [model.name], overrideHides: [], overrideReplaces: [] }];
  }
  const item = {
    id: draft.itemId, name: draft.name.trim(), description: existing?.description ?? "",
    eth_address: (opts.address ?? existing?.eth_address ?? "").toLowerCase(), collection_id: draft.collectionId || null,
    type: "wearable", rarity: draft.rarity, price, beneficiary: existing?.beneficiary ?? null,
    ...(existing?.thumbnail ? { thumbnail: existing.thumbnail } : {}),
    metrics: existing?.metrics ?? emptyMetrics("wearable"), contents, data: itemData("wearable", data),
  };
  const saved = Item.parse(await draftRequest(path, opts, {
    method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ item }),
  }));
  if (model) await uploadItemFiles(path, opts, [model], hashes);
  return { itemId: saved.id, urn: saved.urn ?? "" };
}

const ItemSummary = z.object({ id: z.string().uuid(), name: z.string(), type: z.enum(["wearable", "emote"]), collection_id: z.string().nullable() });
export type ItemDraftSummary = z.infer<typeof ItemSummary>;
export async function deleteWearableDraft(id: string, opts: DraftOptions): Promise<void> {
  await draftRequest(itemPath(id), opts, { method: "DELETE" });
}

export async function listItemDrafts(opts: DraftOptions): Promise<ItemDraftSummary[]> {
  return z.array(ItemSummary).parse(await draftRequest("/v1/drafts/items", opts));
}
