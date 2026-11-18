import { describe, expect, it } from "vitest";
import { fetchCollectionItems, fetchCollectionMeta } from "./collection-detail";

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
