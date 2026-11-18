import { describe, expect, it } from "vitest";

import { fetchWorldsRealm, personalWorldName } from "./deploy-world";
import { loadManageWorlds } from "./manage-worlds.server";

const ADDR = "0x4d02aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa7bf2";
const PERSONAL = `${ADDR}.dcl.eth`;

function stub(handlers: Record<string, () => Response>): typeof fetch {
  return (async (url: string) => {
    for (const [needle, make] of Object.entries(handlers)) {
      if (url.includes(needle)) return make();
    }
    throw new Error(`unstubbed ${url}`);
  }) as unknown as typeof fetch;
}

const json = (body: unknown, status = 200) => () =>
  new Response(JSON.stringify(body), { status });

const realmStatus = (personalWorlds: boolean) =>
  json({
    content: { commitHash: "abc", worldsCount: { ens: 0, dcl: 1 } },
    comms: {
      adapterType: "offline",
      rooms: 0,
      users: 0,
      timestamp: 0,
    },
    personalWorlds: personalWorlds ? { maxWorlds: 100, maxSizeBytes: 52428800 } : null,
    mirror: {
      enabled: false,
      source: null,
      intervalSeconds: null,
      catalogue: null,
      scenes: null,
      worlds: { listed: 0, mirrored: 0, pending: 0, failed: 0, unlisted: 0 },
    },
  });

describe("personalWorldName", () => {
  it("is the lowercase wallet address as a .dcl.eth name, never a NAME label, and never exists for a bad address", () => {
    expect(personalWorldName(ADDR.toUpperCase().replace("0X", "0x"))).toBe(PERSONAL);
    expect(personalWorldName(` ${ADDR} `)).toBe(PERSONAL);
    expect(personalWorldName("")).toBeNull();
    expect(personalWorldName(null)).toBeNull();
    expect(personalWorldName("0x4d02")).toBeNull();
  });
});

describe("fetchWorldsRealm", () => {
  it("reads the personal-world policy from /status, treats another shape as online without it, and rejects a 503", async () => {
    await expect(
      fetchWorldsRealm({ fetchImpl: stub({ "/status": realmStatus(true) }) }),
    ).resolves.toEqual({ online: true, personalWorlds: true });
    await expect(
      fetchWorldsRealm({ fetchImpl: stub({ "/status": realmStatus(false) }) }),
    ).resolves.toEqual({ online: true, personalWorlds: false });
    await expect(
      fetchWorldsRealm({ fetchImpl: stub({ "/status": json({ ok: true }) }) }),
    ).resolves.toEqual({ online: true, personalWorlds: false });
    await expect(
      fetchWorldsRealm({ fetchImpl: stub({ "/status": json({ error: "down" }, 503) }) }),
    ).rejects.toThrow();
  });
});

describe("loadManageWorlds", () => {
  it("lists the personal world next to the owned NAMEs only when the realm hosts them", async () => {
    const data = await loadManageWorlds(ADDR, undefined, {
      fetchImpl: stub({
        "/names": json({ elements: [{ name: "boedo" }] }),
        "/status": realmStatus(true),
        [`/world/${encodeURIComponent(PERSONAL)}/about`]: json(
          { configurations: { scenesUrn: ["urn:1", "urn:2"] } },
        ),
        "/world/boedo.dcl.eth/about": json({ error: "not found" }, 404),
      }),
    });
    const byName = Object.fromEntries(data.worlds.map((w) => [w.name, w]));
    expect(Object.keys(byName).sort()).toEqual([PERSONAL, "boedo.dcl.eth"].sort());
    expect(byName[PERSONAL].deployedScenes).toBe(2);
    expect(byName[PERSONAL].owner).toBe(ADDR);
    expect(byName["boedo.dcl.eth"].deployedScenes).toBe(0);

    const plain = await loadManageWorlds(ADDR, undefined, {
      fetchImpl: stub({
        "/names": json({ elements: [{ name: "boedo" }] }),
        "/status": realmStatus(false),
        "/world/boedo.dcl.eth/about": json({ error: "not found" }, 404),
      }),
    });
    expect(plain.worlds.map((w) => w.name)).toEqual(["boedo.dcl.eth"]);
  });
});
