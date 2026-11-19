import { beforeEach, describe, expect, it } from "vitest";

import fixture from "../../../fixtures/builder-collections.json";
import {
  BuilderCollectionSchema,
  OrphanItemSchema,
  parseCollections,
  parseOrphanItems,
} from "./collections-schema";
import {
  fetchCollections,
  mergeCollectionThumbs,
  readSort,
  readTab,
  readView,
  relativeTime,
  sortCollections,
  tabToUiId,
  toCollectionCard,
  toOrphanItem,
} from "./collections";

const COLLECTIONS = (fixture as { collections: unknown[] }).collections;
const ITEMS = (fixture as { items: unknown[] }).items;

describe("schemas / parsers", () => {
  it("accepts the derived fixture rows and tolerates non-arrays without throwing", () => {
    for (const row of COLLECTIONS) {
      expect(BuilderCollectionSchema.safeParse(row).success).toBe(true);
    }
    for (const row of ITEMS) {
      expect(OrphanItemSchema.safeParse(row).success).toBe(true);
    }
    expect(parseCollections(COLLECTIONS).length).toBe(COLLECTIONS.length);
    expect(parseOrphanItems(ITEMS).length).toBe(ITEMS.length);
    expect(parseCollections(null)).toEqual([]);
    expect(parseOrphanItems(undefined)).toEqual([]);
  });

  it("reads the node's ISO-8601 timestamps as epoch milliseconds", () => {
    const [collection] = parseCollections([
      { ...(COLLECTIONS[0] as object), created_at: "2023-12-01T00:00:00.000Z", updated_at: "2024-03-01T00:00:00.000Z", reviewed_at: null },
    ]);
    expect([collection.created_at, collection.updated_at, collection.reviewed_at]).toEqual([1701388800000, 1709251200000, null]);
    const [item] = parseOrphanItems([{ ...(ITEMS[0] as object), created_at: "2023-12-01T00:00:00.000Z", updated_at: "2024-03-01T00:00:00.000Z" }]);
    expect([item.createdAt, item.updatedAt]).toEqual([1701388800000, 1709251200000]);
  });

  it("refuses a timestamp that is not an ISO-8601 string", () => {
    expect(parseCollections([{ ...(COLLECTIONS[0] as object), created_at: 1701388800000 }])).toEqual([]);
  });
});

describe("when the node answers an address's collection rows", () => {
  let rows: ReturnType<typeof parseCollections>;

  beforeEach(() => {
    rows = parseCollections(COLLECTIONS);
  });

  it("should read every row of the listing", () => {
    expect(rows).toHaveLength(COLLECTIONS.length);
  });

  it("should count a collection's items from the string item_count", () => {
    expect(rows.find((c) => c.name === "Genesis Threads")!.count).toBe(12);
  });

  it("should tell a third-party collection by its urn and read its provider from it", () => {
    const linked = rows.find((c) => c.name === "Aether Linked Wearables")!;
    expect([linked.type, linked.third_party_id, linked.pending]).toEqual(["third_party", "urn:decentraland:matic:collections-thirdparty:aether", true]);
  });

  it("should derive the status dot from the published and approved state", () => {
    expect(rows.map((c) => [c.is_published, c.is_approved, c.status])).toEqual(
      rows.map((c) => [c.is_published, c.is_approved, c.is_approved ? "synced" : c.is_published ? "under_review" : "unsynced"]),
    );
  });

  it("should start every row without thumbnails, which the item rows fill", () => {
    expect(rows.every((c) => c.thumbs.length === 0)).toBe(true);
  });

  it("should fill a collection's thumbnails from the node's item rows, which name it by its builder id", () => {
    const genesis = rows.find((c) => c.name === "Genesis Threads")!;
    const [item] = parseOrphanItems([{ ...(ITEMS[0] as object), collection_id: genesis.id }], "https://builder.test");
    expect(mergeCollectionThumbs(rows, [item]).find((c) => c.id === genesis.id)!.thumbs).toEqual([
      { url: "https://builder.test/v1/storage/contents/bafkreicjyysmiyonyudrmq6ejc6b7m42uma5fcnqoy6h4frcmugd7jtfma", signed: false },
    ]);
  });

  describe("and the item that names the thumbnail is not published", () => {
    let genesisId: string;
    let thumbs: unknown;

    beforeEach(() => {
      genesisId = rows.find((c) => c.name === "Genesis Threads")!.id;
      const [item] = parseOrphanItems([{ ...(ITEMS[0] as object), collection_id: genesisId, is_published: false }], "https://builder.test");
      thumbs = mergeCollectionThumbs(rows, [item]).find((c) => c.id === genesisId)!.thumbs;
    });

    it("should mark the thumbnail as one only a signed request reads", () => {
      expect(thumbs).toEqual([
        { url: "https://builder.test/v1/storage/contents/bafkreicjyysmiyonyudrmq6ejc6b7m42uma5fcnqoy6h4frcmugd7jtfma", signed: true },
      ]);
    });
  });
});

describe("when the node answers an address's item rows", () => {
  let items: ReturnType<typeof parseOrphanItems>;

  beforeEach(() => {
    items = parseOrphanItems(ITEMS);
  });

  it("should read every row", () => {
    expect(items).toHaveLength(ITEMS.length);
  });

  it("should derive each row's status dot from its published and approved state", () => {
    expect(items.map((it) => it.status)).toEqual(["synced", "under_review", "unpublished", "unpublished", "unpublished", "synced"]);
  });

  it("should tell a smart wearable by a script among its contents", () => {
    expect(items.map((it) => it.type)).toEqual(["wearable", "emote", "smart_wearable", "wearable", "wearable", "emote"]);
  });
});

describe("URL readers", () => {
  it("normalises tab / view / sort and maps a tab to its ui3 TABS id", () => {
    expect(readTab("third_party")).toBe("third_party");
    expect(readTab("items")).toBe("items");
    expect(readTab("garbage")).toBe("standard");
    expect(readTab(null)).toBe("standard");
    expect(readView("list")).toBe("list");
    expect(readView("grid")).toBe("grid");
    expect(readView(null)).toBe("grid");
    expect(readSort("CREATED_AT_DESC")).toBe("CREATED_AT_DESC");
    expect(readSort("NAME_ASC")).toBe("NAME_ASC");
    expect(readSort("nope")).toBe("MOST_RELEVANT");
    expect(readSort(null)).toBe("MOST_RELEVANT");
    expect(tabToUiId("standard")).toBe("standard_collections");
    expect(tabToUiId("third_party")).toBe("third_party_collections");
    expect(tabToUiId("items")).toBe("orphan_items");
  });
});

describe("sortCollections", () => {
  it("orders by created_at both ways and by name, and leaves MOST_RELEVANT untouched", () => {
    const rows = parseCollections(COLLECTIONS);
    const desc = sortCollections(rows, "CREATED_AT_DESC");
    for (let i = 1; i < desc.length; i++) {
      expect(desc[i - 1].created_at ?? 0).toBeGreaterThanOrEqual(desc[i].created_at ?? 0);
    }
    const asc = sortCollections(rows, "CREATED_AT_ASC");
    for (let i = 1; i < asc.length; i++) {
      expect(asc[i - 1].created_at ?? 0).toBeLessThanOrEqual(asc[i].created_at ?? 0);
    }
    const names = sortCollections(rows, "NAME_ASC").map((c) => c.name);
    expect([...names].sort((a, b) => a.localeCompare(b))).toEqual(names);
    expect(sortCollections(rows, "MOST_RELEVANT").map((c) => c.id)).toEqual(rows.map((c) => c.id));
  });
});

describe("view-models", () => {
  it("projects collections and orphan items onto ui3 cards with coarse, null-safe relative dates", () => {
    const rows = parseCollections(COLLECTIONS);
    const tp = rows.find((c) => c.type === "third_party")!;
    const std = rows.find((c) => c.type === "standard")!;
    expect(toCollectionCard(tp).type).toBe("third_party");
    expect(toCollectionCard(std).type).toBe("collection");
    const empty = rows.find((c) => c.count === 0)!;
    expect(toCollectionCard(empty).count).toBe(0);
    expect(toCollectionCard(empty).thumbs).toEqual([]);
    const vm = toOrphanItem(parseOrphanItems(ITEMS)[0]);
    expect(vm.id).toBeTruthy();
    expect(typeof vm.createdAt).toBe("string");
    expect(typeof vm.grad).toBe("string");
    expect(relativeTime(null)).toBe("");
    expect(relativeTime(Date.now())).toBe("today");
    expect(relativeTime(Date.now() - 10 * 86_400_000)).toMatch(/week/);
  });
});

describe("fetchCollections (injected fetch, no network)", () => {
  it("unwraps a {data} envelope and validates rows", async () => {
    const stub: typeof fetch = async () =>
      new Response(JSON.stringify({ data: COLLECTIONS }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    const rows = await fetchCollections("0xabc", { fetchImpl: stub });
    expect(rows.length).toBe(COLLECTIONS.length);
    expect(rows[0].name).toBeTruthy();
  });
});
