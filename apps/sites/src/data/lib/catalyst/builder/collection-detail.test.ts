import { beforeEach, describe, expect, it } from "vitest";
import { fetchCollectionItems, fetchCollectionMeta, fetchOnchainCollectionDetail } from "./collection-detail";

const item = {
  id: "draft", name: "Coat", rarity: "rare", type: "wearable",
  price: "2125000000000000001", contents: { "coat.glb": "model-hash" },
  data: { category: "upper_body", representations: [{ mainFile: "coat.glb", contents: ["coat.glb"] }] },
};
async function read(items: unknown[]) {
  return fetchCollectionItems("collection", { fetchImpl: async () => Response.json({ data: items }) });
}

describe("collection item readiness", () => {
  it("displays exact MANA rather than wei, including free items", async () => {
    const { wearables } = await read([item, { ...item, price: "0" }]);
    expect(wearables.map(i => [i.price, i.status])).toEqual([["2.125000000000000001", "ready"], ["0", "ready"]]);
  });
  it("does not mark a priced draft ready when metadata or resources are missing", async () => {
    const { wearables } = await read([
      { ...item, data: null }, { ...item, data: { category: "upper_body" } },
      { ...item, contents: {} }, { ...item, price: null },
      { ...item, data: { ...item.data, representations: [{ mainFile: "coat.glb", contents: [] }] } },
      { ...item, data: { representations: item.data.representations } },
    ]);
    expect(wearables.map(i => i.status)).toEqual(Array(6).fill("not_ready"));
  });
  it("preserves on-chain review status even when draft resources are unavailable", async () => {
    const { wearables } = await read([
      { ...item, contents: {}, is_published: true },
      { ...item, contents: {}, is_published: true, is_approved: true },
    ]);
    expect(wearables.map(i => i.status)).toEqual(["under_review", "published"]);
  });
});

describe("collection metadata", () => {
  it("reads the node's ISO-8601 timestamps as epoch milliseconds", async () => {
    const meta = await fetchCollectionMeta("collection", {
      fetchImpl: async () => Response.json({ ok: true, data: {
        id: "collection", name: "Hats", eth_address: "0x1111111111111111111111111111111111111111", contract_address: null,
        urn: null, third_party_id: null, is_published: false, is_approved: false,
        created_at: "2023-12-01T00:00:00.000Z", updated_at: "2024-03-01T00:00:00.000Z",
      } }),
    });
    expect(meta).toMatchObject({ name: "Hats", createdAt: 1701388800000, updatedAt: 1709251200000 });
  });
});

describe("when a collection on chain is opened by its contract address", () => {
  const contract = "0x5eed000000000000000000000000000000000302";
  const collectionId = "c011ec71-0000-4000-8000-000000000302";
  let detail: Awaited<ReturnType<typeof fetchOnchainCollectionDetail>>;

  beforeEach(async () => {
    const minted = {
      id: "c011ec71-0000-4000-8000-0000000003b1", name: "Minted hat", description: null, collection_id: collectionId,
      blockchain_item_id: "0", price: "2000000000000000000", rarity: "rare", type: "wearable", total_supply: 3,
      data: { category: "hat", hides: [], replaces: [], tags: [], representations: [] }, contents: {}, is_published: true, is_approved: false,
    };
    const other = { ...minted, id: "c011ec71-0000-4000-8000-0000000003a1", name: "Elsewhere", collection_id: null };
    detail = await fetchOnchainCollectionDetail("0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e", contract, {
      fetchImpl: async (url) => Response.json({ ok: true, data: String(url).endsWith("/items") ? [minted, other] : [{
        id: collectionId, name: "Items on chain", contract_address: contract, is_published: true, is_approved: false,
        urn: `urn:decentraland:matic:collections-v2:${contract}`, created_at: "2025-12-10T00:00:00.000Z", updated_at: "2025-12-10T00:00:00.000Z", reviewed_at: null,
      }] }),
    });
  });

  it("should list the items the node's rows name by the collection's builder id, with their category", () => {
    expect(detail.wearables.map(it => [it.name, it.category, it.rarity, it.price, it.supply])).toEqual([["Minted hat", "hat", "rare", "2", "3"]]);
  });
});
