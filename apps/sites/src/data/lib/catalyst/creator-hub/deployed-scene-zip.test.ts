import { strFromU8, Unzip, unzipSync } from "fflate";
import { afterEach, describe, expect, it, vi } from "vitest";

import { deployedSceneZip } from "./deployed-scene-zip";

const ENTITY = {
  type: "scene",
  pointers: ["10,10"],
  content: [{ file: "bin/index.js", hash: "bafjs" }],
  metadata: { display: { title: "Code Scene" }, scene: { base: "10,10" } },
};

afterEach(() => vi.unstubAllGlobals());

function entryNames(bytes: Uint8Array): string[] {
  const names: string[] = [];
  const unzip = new Unzip((file) => names.push(file.name));
  unzip.push(bytes, true);
  return names;
}

describe("deployedSceneZip", () => {
  describe("when the entity is a deployed scene", () => {
    it("should zip its files under a slug of its title and add the scene.json it lacks", async () => {
      vi.stubGlobal("fetch", vi.fn(async (url: string) =>
        url.endsWith("/content/contents/baf-entity") ? Response.json(ENTITY) : new Response("console.log(1)"),
      ));

      const zip = await deployedSceneZip("baf-entity", "My Scene!");

      expect(zip?.name).toBe("my-scene.zip");
      const files = unzipSync(zip!.bytes);
      expect(strFromU8(files["bin/index.js"]!)).toBe("console.log(1)");
      expect(JSON.parse(strFromU8(files["scene.json"]!))).toEqual(ENTITY.metadata);
    });
  });

  describe("when a deployed file is named __proto__", () => {
    it("should zip it as an ordinary file", async () => {
      const entity = { ...ENTITY, content: [{ file: "__proto__", hash: "bafproto" }, { file: "scene.json", hash: "bafscene" }] };
      vi.stubGlobal("fetch", vi.fn(async (url: string) =>
        url.endsWith("/content/contents/baf-entity") ? Response.json(entity) : new Response("abcde"),
      ));

      const zip = await deployedSceneZip("baf-entity");

      expect(entryNames(zip!.bytes).sort()).toEqual(["__proto__", "scene.json"]);
    });
  });

  describe("when the entity is not a scene", () => {
    it("should return null", async () => {
      vi.stubGlobal("fetch", vi.fn(async () => Response.json({ ...ENTITY, type: "profile" })));

      expect(await deployedSceneZip("baf-entity")).toBeNull();
    });
  });
});
