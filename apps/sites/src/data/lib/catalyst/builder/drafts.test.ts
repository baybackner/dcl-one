import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { hashV1 } from "@ui/shop/seller/lib/entity";
import { deleteWearableDraft, listItemDrafts, loadWearableDraft, saveWearableDraft, type WearableDraft } from "./drafts";

const id = "fd84234c-55d4-40a8-afbc-ea5a6fe00761";
it("surfaces a refused deletion and never treats an invalid id as a path", async () => {
  const fetch = vi.fn(async (_url: string, _init?: RequestInit) => Response.json({ ok: false, data: { id }, error: "The collection for the item is locked. The item can't be deleted." }, { status: 423 }));
  await expect(deleteWearableDraft(id, { fetch })).rejects.toThrow("The collection for the item is locked. The item can't be deleted.");
  expect(fetch.mock.calls[0][1]).toMatchObject({ method: "DELETE", cache: "no-store" });
  await expect(deleteWearableDraft("../collections", { fetch })).rejects.toThrow("Invalid wearable draft id");
  expect(fetch).toHaveBeenCalledTimes(1);
});
it("shows the node's explanation when its creator writes are off", async () => {
  const message = "creator writes are disabled on this node (CREATE_WRITES_ENABLED=false)";
  const fetch = vi.fn(async () => Response.json({ ok: false, data: {}, error: message }, { status: 503 }));
  await expect(deleteWearableDraft(id, { fetch })).rejects.toThrow(message);
});
it("lists local drafts through the signed owner endpoint", async () => {
  const item = { id, name: "My draft", type: "wearable", collection_id: null };
  const fetch = vi.fn(async () => Response.json({ data: [item] }));
  expect(await listItemDrafts({ fetch, base: "https://builder.test" })).toEqual([item]);
  expect(fetch).toHaveBeenCalledWith("https://builder.test/v1/drafts/items", expect.objectContaining({ cache: "no-store" }));
});

const stored = {
  id, name: "Jacket", type: "wearable", collection_id: null, description: "Keep this",
  thumbnail: "thumbnail.png", rarity: "epic", price: "1500000000000000000",
  beneficiary: "0x1111111111111111111111111111111111111111", is_published: false,
  data: { category: "upper_body", tags: ["original"], representations: [{ mainFile: "jacket.glb", bodyShapes: ["female"] }] },
  contents: { "jacket.glb": "old-cid", "thumbnail.png": "image-cid" }, urn: null,
};
const draft: WearableDraft = {
  itemId: id, collectionId: "", name: "Edited jacket", modelFile: "jacket.glb",
  category: "upper_body", rarity: "rare", price: "2.000000000000000001", free: false,
};
const ok = (data: unknown) => Response.json({ data });
function model(): File {
  const bytes = new Uint8Array(12);
  const header = new DataView(bytes.buffer);
  header.setUint32(0, 0x46546c67, true); header.setUint32(4, 2, true); header.setUint32(8, bytes.length, true);
  return new File([bytes], "new.glb");
}

describe("wearable persistence", () => {
  it("reads stored values with exact MANA conversion", async () => {
    const fetch = vi.fn().mockResolvedValue(ok(stored));
    expect(await loadWearableDraft(id, { fetch, base: "https://builder.test" })).toEqual({
      itemId: id, collectionId: "", name: "Jacket", modelFile: "jacket.glb", category: "upper_body",
      rarity: "epic", price: "1.5", free: false,
    });
    expect(fetch).toHaveBeenCalledWith(`https://builder.test/v1/items/${id}`, expect.objectContaining({ cache: "no-store" }));
  });

  it("preserves unrelated metadata and content on an existing-item edit", async () => {
    const fetch = vi.fn().mockResolvedValueOnce(ok(stored)).mockImplementationOnce(async (_url, init) => ok({ ...stored, ...JSON.parse(init.body).item }));
    const result = await saveWearableDraft(draft, { fetch });
    expect(result).toEqual({ itemId: id, urn: "" });
    const item = JSON.parse(fetch.mock.calls[1][1].body).item;
    expect(item).toMatchObject({ description: stored.description, thumbnail: stored.thumbnail, beneficiary: stored.beneficiary,
      contents: stored.contents, data: { category: "upper_body", tags: ["original"] }, price: "2000000000000000001", name: draft.name, rarity: "rare" });
  });

  it("rejects glTF external assets instead of saving a broken model", async () => {
    const fetch = vi.fn();
    const file = new File([JSON.stringify({ asset: { version: "2.0" }, buffers: [{ uri: "missing.bin" }] })], "scene.gltf");
    await expect(saveWearableDraft({ ...draft, model: file }, { fetch })).rejects.toThrow("references separate files");
    expect(fetch).not.toHaveBeenCalled();
  });

  it.each(["-1", "1e9", "1.1234567890123456789", "NaN"])("rejects invalid price %s before writing", async price => {
    const fetch = vi.fn();
    await expect(saveWearableDraft({ ...draft, price }, { fetch })).rejects.toThrow("MANA price");
    expect(fetch).not.toHaveBeenCalled();
  });

  it("rejects fake models and does not create an item after a permission error", async () => {
    const fetch = vi.fn().mockResolvedValue(Response.json({ ok: false, data: { id }, error: "Unauthorized" }, { status: 401 }));
    await expect(saveWearableDraft({ ...draft, model: new File(["not a model"], "fake.glb") }, { fetch })).rejects.toThrow("GLB");
    expect(fetch).not.toHaveBeenCalled();
    await expect(saveWearableDraft(draft, { fetch })).rejects.toThrow("Unauthorized");
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});

type DraftFetch = (url: string, init?: RequestInit) => Promise<Response>;
const signer = "0x2222222222222222222222222222222222222222";

describe("when a new wearable is saved with its model", () => {
  let fetch: Mock<DraftFetch>;
  let file: File;
  let hash: string;

  beforeEach(async () => {
    file = model();
    hash = await hashV1(new Uint8Array(await file.arrayBuffer()));
    fetch = vi.fn<DraftFetch>(async (_url, init) => {
      if (!init?.method) return Response.json({ ok: false, data: {}, error: "Couldn't find the item" }, { status: 404 });
      if (init.method === "POST") return Response.json({ ok: true });
      return ok({ ...stored, ...JSON.parse(init.body as string).item });
    });
    await saveWearableDraft({ ...draft, name: "Fresh jacket", model: file, modelFile: file.name }, { fetch, address: signer });
  });

  it("should save the item naming the model's content hash before uploading it", () => {
    expect(fetch.mock.calls.map(([, init]) => init?.method ?? "GET")).toEqual(["GET", "PUT", "POST"]);
  });

  it("should send every key upstream's item schema requires", () => {
    const item = JSON.parse(fetch.mock.calls[1][1]!.body as string).item;
    expect(item).toEqual({
      id, name: "Fresh jacket", description: "", eth_address: signer, collection_id: null, type: "wearable", rarity: "rare",
      price: "2000000000000000001", beneficiary: null,
      metrics: { meshes: 0, bodies: 0, materials: 0, textures: 0, triangles: 0, entities: 0 },
      contents: { "new.glb": hash },
      data: { category: "upper_body", hides: [], replaces: [], tags: [], representations: [{
        bodyShapes: ["urn:decentraland:off-chain:base-avatars:BaseMale", "urn:decentraland:off-chain:base-avatars:BaseFemale"],
        mainFile: "new.glb", contents: ["new.glb"], overrideHides: [], overrideReplaces: [] }] },
    });
  });

  it("should upload the model's bytes under its content hash", async () => {
    const form = fetch.mock.calls[2][1]!.body as FormData;
    expect([[...form.keys()], new Uint8Array(await (form.get(hash) as File).arrayBuffer())]).toEqual([[hash], new Uint8Array(await file.arrayBuffer())]);
  });
});

describe("when the model of an existing wearable is replaced", () => {
  let fetch: Mock<DraftFetch>;
  let hash: string;

  beforeEach(async () => {
    const file = model();
    hash = await hashV1(new Uint8Array(await file.arrayBuffer()));
    fetch = vi.fn<DraftFetch>(async (_url, init) => {
      if (!init?.method) return ok(stored);
      if (init.method === "POST") return Response.json({ ok: true });
      return ok({ ...stored, ...JSON.parse(init.body as string).item });
    });
    await saveWearableDraft({ ...draft, model: file }, { fetch, address: signer });
  });

  it("should keep the body shapes and the other files while naming the new model", () => {
    const item = JSON.parse(fetch.mock.calls[1][1]!.body as string).item;
    expect([item.data.representations[0], item.contents]).toEqual([
      { bodyShapes: ["female"], mainFile: "new.glb", contents: ["new.glb"], overrideHides: [], overrideReplaces: [] },
      { "jacket.glb": "old-cid", "thumbnail.png": "image-cid", "new.glb": hash },
    ]);
  });
});

describe("when the upload of a saved wearable's model fails", () => {
  let fetch: Mock<DraftFetch>;

  beforeEach(() => {
    fetch = vi.fn<DraftFetch>(async (_url, init) => {
      if (!init?.method) return ok(stored);
      if (init.method === "POST") return Response.json({ ok: false, data: {}, error: "Upload unavailable" }, { status: 503 });
      return ok({ ...stored, ...JSON.parse(init.body as string).item });
    });
  });

  it("should report the failure instead of success", async () => {
    await expect(saveWearableDraft({ ...draft, model: model() }, { fetch, address: signer })).rejects.toThrow("Upload unavailable");
  });
});

describe("when a wearable's name doesn't fit upstream's item schema", () => {
  let fetch: Mock<DraftFetch>;

  beforeEach(() => {
    fetch = vi.fn<DraftFetch>();
  });

  it.each(["A name longer than thirty-two chars", "Colon: hat"])("should refuse %s before writing", async name => {
    await expect(saveWearableDraft({ ...draft, name }, { fetch })).rejects.toThrow("32 characters");
    expect(fetch).not.toHaveBeenCalled();
  });
});
