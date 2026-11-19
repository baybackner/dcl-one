import { z } from "zod";
import { warnInvalid } from "../warn";
import type { FullItemOut as RsFullItemOut } from "@catalyst-api/builder/FullItemOut";
import type { CollectionListingRowOut as RsCollectionListingRowOut } from "@catalyst-api/builder/CollectionListingRowOut";

const nullableStr = z.string().nullish().transform((v) => v ?? null);
/** A builder timestamp (ISO-8601, as the node writes it) as epoch milliseconds. */
export const isoMillis = z.iso.datetime().nullish().transform((v) => (v ? Date.parse(v) : null));

export const COLLECTION_TYPES = ["standard", "third_party"] as const;
export type CollectionType = (typeof COLLECTION_TYPES)[number];

const COLLECTION_STATUSES = [
  "synced",
  "under_review",
  "unsynced",
  "loading",
] as const;
export type CollectionStatus = (typeof COLLECTION_STATUSES)[number];

export const COLLECTION_SORTS = [
  "MOST_RELEVANT",
  "CREATED_AT_DESC",
  "CREATED_AT_ASC",
  "NAME_DESC",
  "NAME_ASC",
  "UPDATED_AT_DESC",
  "UPDATED_AT_ASC",
] as const;
export type CollectionSort = (typeof COLLECTION_SORTS)[number];

const THIRD_PARTY_URN = /^(urn:decentraland:[^:]+:collections-thirdparty:[^:]+):[^:]+$/;

function statusOf(row: { is_published: boolean; is_approved: boolean }): CollectionStatus {
  if (row.is_approved) return "synced";
  return row.is_published ? "under_review" : "unsynced";
}

/** A row of `GET /v1/{address}/collections` (builder-server's `FullCollection`), as the list shows it. */
export const BuilderCollectionSchema = z
  .object({
    id: z.string(),
    name: z.string(),
    is_published: z.boolean(),
    is_approved: z.boolean(),
    reviewed_at: isoMillis,
    created_at: isoMillis,
    updated_at: isoMillis,
    contract_address: nullableStr,
    urn: z.string(),
    lock: z.string().nullable(),
    item_count: z.string().regex(/^\d+$/),
  })
  .transform((row) => {
    const provider = THIRD_PARTY_URN.exec(row.urn)?.[1] ?? null;
    const type: CollectionType = provider ? "third_party" : "standard";
    return {
      id: row.id,
      name: row.name,
      type,
      is_published: row.is_published,
      is_approved: row.is_approved,
      reviewed_at: row.reviewed_at,
      created_at: row.created_at,
      updated_at: row.updated_at,
      contract_address: row.contract_address,
      third_party_id: provider,
      urn: row.urn,
      status: statusOf(row),
      pending: row.lock !== null,
      count: Number(row.item_count),
      thumbs: [] as { url: string; signed: boolean }[],
    };
  });
export type BuilderCollection = z.infer<typeof BuilderCollectionSchema>;

const ORPHAN_ITEM_TYPES = ["wearable", "emote", "smart_wearable"] as const;

const ORPHAN_ITEM_STATUSES = [
  "synced",
  "under_review",
  "unsynced",
  "loading",
  "unpublished",
] as const;

/** A row of `GET /v1/{address}/items`: builder-server's item body, read for the list's item card. */
export const OrphanItemSchema = z.object({
  id: z.string(),
  name: z.string(),
  type: z.string(),
  collection_id: nullableStr,
  created_at: isoMillis,
  updated_at: isoMillis,
  thumbnail: nullableStr,
  contents: z.record(z.string(), z.string()),
  is_published: z.boolean(),
  is_approved: z.boolean(),
});
export type OrphanItemRow = z.infer<typeof OrphanItemSchema>;

/** The list's item card of `row`; `storage` is the node's base, which serves an item's files by hash. */
export function orphanItem(row: OrphanItemRow, storage = ""): OrphanItem {
  const thumbnail = row.thumbnail ? row.contents[row.thumbnail] : undefined;
  return {
    id: row.id,
    name: row.name,
    type: row.type === "emote" ? "emote" : Object.keys(row.contents).some((file) => file.endsWith(".js")) ? "smart_wearable" : "wearable",
    status: row.is_published ? (row.is_approved ? "synced" : "under_review") : "unpublished",
    createdAt: row.created_at,
    updatedAt: row.updated_at,
    grad: null,
    image: thumbnail ? `${storage}/v1/storage/contents/${encodeURIComponent(thumbnail)}` : null,
    imageSigned: !row.is_published,
    collection_id: row.collection_id,
  };
}

export type OrphanItem = {
  id: string;
  name: string;
  type: (typeof ORPHAN_ITEM_TYPES)[number];
  status: (typeof ORPHAN_ITEM_STATUSES)[number];
  createdAt: number | null;
  updatedAt: number | null;
  grad: string | null;
  image: string | null;
  imageSigned: boolean;
  collection_id: string | null;
};

function parseRows<S extends z.ZodTypeAny>(
  schema: S,
  kind: string,
  raw: unknown,
): z.infer<S>[] {
  if (!Array.isArray(raw)) {
    warnInvalid(`${kind} list`, "not an array");
    return [];
  }
  const out: z.infer<S>[] = [];
  for (const row of raw) {
    const r = schema.safeParse(row);
    if (r.success) out.push(r.data);
    else warnInvalid(kind, r.error.issues);
  }
  return out;
}

export function parseCollections(raw: unknown): BuilderCollection[] {
  return parseRows(BuilderCollectionSchema, "BuilderCollection", raw);
}

export function parseOrphanItems(raw: unknown, storage = ""): OrphanItem[] {
  return parseRows(OrphanItemSchema, "OrphanItem", raw).map((row) => orphanItem(row, storage));
}

type AssignableTo<Sub, Sup> = Sub extends Sup ? true : false;

type Assert<T extends true> = T;

export type _DriftBuilderCollection = Assert<
  AssignableTo<RsCollectionListingRowOut, z.input<typeof BuilderCollectionSchema>>
>;

export type _DriftOrphanItem = Assert<
  AssignableTo<RsFullItemOut, z.input<typeof OrphanItemSchema>>
>;

