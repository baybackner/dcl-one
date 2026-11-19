import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { decodeFunctionData, parseAbi } from "viem";
import { hashV1 } from "@ui/shop/seller/lib/entity";
import { collectionDraftWriter, listCollectionDrafts } from "./collection-drafts";

it("lists local collections through the signed owner endpoint", async () => {
  const collection = { id: "fd84234c-55d4-40a8-afbc-ea5a6fe00761", name: "My collection", is_published: false };
  const fetch = vi.fn(async () => Response.json({ data: [collection] }));
  expect(await listCollectionDrafts({ fetch, base: "https://builder.test" })).toEqual([collection]);
  expect(fetch).toHaveBeenCalledWith("https://builder.test/v1/drafts/collections", expect.objectContaining({ cache: "no-store" }));
});

it("retries an interrupted collection import without duplicating items or erasing saved content", async () => {
  const collectionIds: string[] = [];
  const itemIds = new Set<string>();
  const items = new Map<string, Record<string, unknown>>();
  let fail = true;
  const fetch = vi.fn(async (url: string, init?: RequestInit) => {
    const path = new URL(url).pathname;
    if (path.includes('/collections/')) {
      const collection = JSON.parse(init!.body as string).collection;
      collectionIds.push(collection.id);
      return Response.json({ data: { ...collection, is_published: false } });
    }
    const id = path.split('/')[3];
    itemIds.add(id);
    if (init?.method === 'PUT') {
      const item = JSON.parse(init.body as string).item;
      items.set(id, item);
      return Response.json({ data: item });
    }
    if (init?.method === 'POST') {
      if (fail) { fail = false; return Response.json({ message: 'Upload offline' }, { status: 503 }); }
      return Response.json({ ok: true });
    }
    return items.has(id) ? Response.json({ data: items.get(id) }) : Response.json({ message: 'Item not found' }, { status: 404 });
  });
  const writer = collectionDraftWriter(() => ({ fetch, address: '0x1111111111111111111111111111111111111111', base: 'https://builder.test' }));
  const args = { name: 'Saved collection', type: 'standard' as const, items: [{ file: new File([JSON.stringify({ asset: { version: '2.0' } })], 'item.gltf') }] };
  await expect(writer(args)).rejects.toThrow('Upload offline');
  const result = await writer(args);
  expect(new Set(collectionIds).size).toBe(1);
  expect(itemIds.size).toBe(1);
  expect(result).toMatchObject({ collectionId: collectionIds[0], contractAddress: '', simulated: false });
  expect([...items.values()][0].contents).toEqual({ 'item.gltf': await hashV1(new TextEncoder().encode(JSON.stringify({ asset: { version: '2.0' } }))) });
  expect(fetch.mock.calls.filter(([url, init]) => url.includes('/items/') && init?.method === 'PUT')).toHaveLength(2);
});

it("validates every imported item before creating any server records", async () => {
  const fetch = vi.fn();
  const writer = collectionDraftWriter(() => ({ fetch }));
  await expect(writer({ name: 'Draft', type: 'standard', items: [{ file: new File([JSON.stringify({ asset: { version: '2.0' } })], 'valid.gltf') }, {}] })).rejects.toThrow('original item files');
  expect(fetch).not.toHaveBeenCalled();
});

it("saves linked drafts with the selected managed provider and refuses revoked access before writing", async () => {
  const provider = "urn:decentraland:matic:collections-thirdparty:example";
  let managed = true;
  const collections: Record<string, string>[] = [];
  const fetch = vi.fn(async (url: string, init?: RequestInit) => {
    if (url.endsWith("/thirdParties")) return Response.json({ data: [{ id: provider, name: "Example", managers: managed ? ["0xowner"] : ["0xother"], published: true }] });
    if (url.includes("/collections/")) {
      const collection = JSON.parse(init!.body as string).collection;
      collections.push(collection);
      return Response.json({ data: { ...collection, is_published: false } });
    }
    if (init?.method === "POST") return Response.json({ ok: true });
    return init?.method === "PUT" ? Response.json({ data: {} }) : Response.json({ error: "Missing" }, { status: 404 });
  });
  const writer = collectionDraftWriter(() => ({ fetch, address: "0xOwner", base: "https://local.test" }));
  const args = { name: "Linked draft", type: "linked" as const, thirdPartyId: provider, items: [{ file: new File([JSON.stringify({ asset: { version: "2.0" } })], "item.gltf") }] };
  const result = await writer(args);
  expect(result.simulated).toBe(false);
  expect(collections[0].urn).toBe(`${provider}:${result.collectionId}`);
  managed = false;
  await expect(writer(args)).rejects.toThrow("no longer manages");
  expect(collections).toHaveLength(1);
  await expect(writer({ ...args, thirdPartyId: "https://evil.test" })).rejects.toThrow("Choose a provider");
});

type DraftFetch = (url: string, init?: RequestInit) => Promise<Response>;

describe("when a standard collection is saved", () => {
  const owner = "0xC1aba94a79ca07d9d15a62ff5c9b382777e0c70e";
  let bodies: { collection: Record<string, unknown>; data?: string }[];
  let fetch: Mock<DraftFetch>;

  beforeEach(async () => {
    bodies = [];
    fetch = vi.fn<DraftFetch>(async (url, init) => {
      const path = new URL(url).pathname;
      if (path.startsWith("/v1/collections/")) {
        const body = JSON.parse(init!.body as string);
        bodies.push(body);
        return Response.json({ data: { ...body.collection, is_published: false, is_approved: false, item_count: "0", urn: "urn:decentraland:matic:collections-v2:0x969194d18107025936324ddc6b1019a6755c94d5" } });
      }
      if (init?.method === "POST") return Response.json({ ok: true });
      return init?.method === "PUT" ? Response.json({ data: {} }) : Response.json({ error: "Item not found" }, { status: 404 });
    });
    await collectionDraftWriter(() => ({ fetch, address: owner, base: "https://builder.test" }))({
      name: "  Hats  ", type: "standard", items: [{ file: new File([JSON.stringify({ asset: { version: "2.0" } })], "item.gltf") }],
    });
  });

  it("should send the keys the collection schema requires, with the signing wallet as owner", () => {
    expect(bodies[0].collection).toEqual({ id: expect.any(String), name: "Hats", eth_address: owner.toLowerCase(), salt: null, contract_address: null });
  });

  it("should send initialize calldata naming the signing wallet as the collection's creator", () => {
    const abi = parseAbi(["function initialize(string name,string symbol,string baseURI,address creator,bool shouldComplete,bool isApproved,address rarities,(string rarity,uint256 price,address beneficiary,string metadata)[] items)"]);
    const { args } = decodeFunctionData({ abi, data: bodies[0].data as `0x${string}` });
    expect([args[0], args[1], args[3].toLowerCase(), args[4], args[5], args[7]]).toEqual(["Hats", "DCL-HTS", owner.toLowerCase(), true, false, []]);
  });
});

describe("when a standard collection is saved without a signed-in wallet", () => {
  let fetch: Mock<DraftFetch>;

  beforeEach(() => {
    fetch = vi.fn<DraftFetch>();
  });

  it("should refuse before writing anything", async () => {
    const writer = collectionDraftWriter(() => ({ fetch }));
    await expect(writer({ name: "Hats", type: "standard", items: [{ file: new File([JSON.stringify({ asset: { version: "2.0" } })], "item.gltf") }] })).rejects.toThrow("Sign in");
  });
});

describe("when a collection's imported item is saved", () => {
  const owner = "0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e";
  const model = JSON.stringify({ asset: { version: "2.0" } });
  let fetch: Mock<DraftFetch>;
  let itemCalls: Parameters<DraftFetch>[];

  beforeEach(async () => {
    fetch = vi.fn<DraftFetch>(async (url, init) => {
      if (new URL(url).pathname.startsWith("/v1/collections/")) {
        return Response.json({ data: { ...JSON.parse(init!.body as string).collection, is_published: false, item_count: "0" } });
      }
      if (init?.method === "POST") return Response.json({ ok: true });
      return init?.method === "PUT" ? Response.json({ data: JSON.parse(init.body as string).item }) : Response.json({ error: "Item not found" }, { status: 404 });
    });
    await collectionDraftWriter(() => ({ fetch, address: owner, base: "https://builder.test" }))({
      name: "Hats", type: "standard", items: [{ file: new File([model], "item.gltf") }],
    });
    itemCalls = fetch.mock.calls.filter(([url]) => new URL(url).pathname.startsWith("/v1/items/"));
  });

  it("should save the item naming each file's content hash and then upload the files under those hashes", async () => {
    const hash = await hashV1(new TextEncoder().encode(model));
    const item = JSON.parse(itemCalls[0][1]!.body as string).item;
    const form = itemCalls[1][1]!.body as FormData;
    expect([itemCalls.map(([, init]) => init?.method), item.contents, [...form.keys()]]).toEqual([["PUT", "POST"], { "item.gltf": hash }, [hash]]);
  });

  it("should send the keys upstream's item schema requires", () => {
    const item = JSON.parse(itemCalls[0][1]!.body as string).item;
    expect([item.eth_address, item.metrics, item.data.category, item.data.hides, item.data.replaces, item.data.tags]).toEqual([
      owner, { meshes: 0, bodies: 0, materials: 0, textures: 0, triangles: 0, entities: 0 }, "upper_body", [], [], [],
    ]);
  });
});

describe("when an imported item's name doesn't fit upstream's item schema", () => {
  let fetch: Mock<DraftFetch>;

  beforeEach(() => {
    fetch = vi.fn<DraftFetch>();
  });

  it("should refuse before writing anything", async () => {
    const writer = collectionDraftWriter(() => ({ fetch, address: "0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e" }));
    await expect(writer({ name: "Hats", type: "standard", items: [{ file: new File([JSON.stringify({ asset: { version: "2.0" } })], "a-model-name-longer-than-thirty-two.gltf") }] })).rejects.toThrow("32 characters");
    expect(fetch).not.toHaveBeenCalled();
  });
});
