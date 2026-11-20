import { dehydrate, QueryClient } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";

import { destination } from "./destination.fixture";
import type { Destination, DestinationPage } from "./destinations";
import {
  browsePageRows,
  effectiveSearch,
  fetchPlaceCreators,
  liveMinUsers,
  liveMinUsersFrom,
  nextBrowseOffset,
  parseCategory,
  parseSection,
  placeCreatorAddress,
  placeCreatorsQuery,
  placesFromDehydrated,
  placesKeys,
  selectLiveRail,
} from "./discover";

function page(data: Destination[], total = data.length, received = data.length): DestinationPage {
  return { data, total, received, copy: null };
}

afterEach(() => vi.unstubAllGlobals());

describe("when parsing the /places URL", () => {
  it("should accept Featured and known categories case-insensitively and read anything else as All", () => {
    expect(parseCategory(" Music ")).toBe("music");
    expect(parseCategory("FEATURED")).toBe("featured");
    expect(parseCategory("casino")).toBe("all");
    expect(parseCategory(null)).toBe("all");
  });

  it("should read both spellings of favourites and default unknown tabs to Explore all", () => {
    expect(parseSection("favourites")).toBe("favourites");
    expect(parseSection("favorites")).toBe("favourites");
    expect(parseSection("my")).toBe("my");
    expect(parseSection("admin")).toBe("all");
    expect(parseSection(null)).toBe("all");
  });

  it("should drop a search shorter than three characters, which places-api rejects", () => {
    expect(effectiveSearch("  ab ")).toBe("");
    expect(effectiveSearch(" abc ")).toBe("abc");
    expect(effectiveSearch(undefined)).toBe("");
  });
});

describe("when keying the places queries", () => {
  it("should give every category and search its own browse entry", () => {
    const base = placesKeys.browse({ category: "all", search: "" });
    expect(placesKeys.browse({ category: "music", search: "" })).not.toEqual(base);
    expect(placesKeys.browse({ category: "all", search: "plaza" })).not.toEqual(base);
  });

  it("should put member reads under the private prefix of the lowercased account", () => {
    expect(placesKeys.favourites("0xABC")).toEqual(["private", "0xabc", "places", "favourites"]);
    expect(placesKeys.mine("0xABC")).toEqual(["private", "0xabc", "places", "mine"]);
  });

  it("should share one creators entry whatever the order or case of the addresses", () => {
    expect(placeCreatorsQuery(["0xB", "0xa", "0xA"]).queryKey).toEqual(placeCreatorsQuery(["0xa", "0xb"]).queryKey);
  });
});

describe("when paging the browse feed", () => {
  it("should continue after a full page while the total has more", () => {
    expect(nextBrowseOffset(page([], 200, 48), 0)).toBe(48);
  });

  it("should stop after a short page even when the total claims more", () => {
    expect(nextBrowseOffset(page([], 200, 10), 48)).toBeUndefined();
  });

  it("should stop once the offset reaches the total", () => {
    expect(nextBrowseOffset(page([], 96, 48), 48)).toBeUndefined();
  });
});

describe("when a page lost rows to schema drift", () => {
  it("should continue at the raw page boundary, not at the parsed row count", () => {
    expect(nextBrowseOffset(page([destination()], 200, 48), 48)).toBe(96);
  });
});

describe("when building the grid rows", () => {
  it("should drop rows repeated across pages and unfinished catalogue entries", () => {
    const rows = browsePageRows(
      [
        page([destination({ id: "a" }), destination({ id: "road", title: "Road at 1,2", categories: [] })]),
        page([destination({ id: "a" }), destination({ id: "b" })]),
      ],
      { category: "all", search: "" },
    );
    expect(rows.flat().map((row) => row.id)).toEqual(["a", "b"]);
  });

  it("should retain a curated placeholder under Featured and hide it on the default view", () => {
    const curated = destination({ id: "curated", image: null, highlighted: true });
    expect(
      browsePageRows([page([curated])], { category: "featured", search: "" })
        .flat()
        .map((row) => row.id),
    ).toEqual(["curated"]);
    expect(browsePageRows([page([curated])], { category: "all", search: "" })).toEqual([[]]);
  });
  it.each([null, "https://example.com/v2/map.png", "https://example.com/bafkreidj26s7aenyxfthfdibnqonzqm5ptc4iamml744gmcyuokewkr76y"])(
    "should preserve explicit search and category matches with cover %s",
    (image) => {
      const row = destination({ image });
      for (const filters of [
        { category: "all", search: "plaza" },
        { category: "music", search: "" },
      ] as const) {
        expect(browsePageRows([page([row])], filters)).toEqual([[row]]);
      }
      expect(browsePageRows([page([row])], { category: "all", search: "" })).toEqual([[]]);
    },
  );
});

describe("when choosing Live Now", () => {
  it("should keep the four busiest visible scenes, busiest first", () => {
    const rows = [1, 9, 4, 7, 3].map((count) => destination({ id: `p${count}`, user_count: count }));
    rows.push(destination({ id: "junk", title: "Empty", user_count: 50 }));
    expect(selectLiveRail(rows).map((row) => row.id)).toEqual(["p9", "p7", "p4", "p3"]);
  });

  it("should keep empty scenes by default, so a quiet node still shows the most active ones", () => {
    const plaza = destination({ id: "plaza", user_count: 0 });
    const unknown = destination({ id: "unknown", user_count: null });
    expect(selectLiveRail([plaza, unknown]).map((row) => row.id)).toEqual(["plaza", "unknown"]);
  });

  it("should exclude a negative head count", () => {
    expect(selectLiveRail([destination({ user_count: -1 })])).toEqual([]);
  });

  it("should still hide entries the grid hides when every scene is empty", () => {
    expect(selectLiveRail([destination({ title: "Empty", user_count: 0 })])).toEqual([]);
  });

  it("should leave out scenes under the minimum head count", () => {
    expect(selectLiveRail([destination({ user_count: 1 })], 2)).toEqual([]);
  });
});

describe("when reading Live Now's head count from the dapps-places-live-min-user variant", () => {
  it.each([
    ["5", 5],
    ["0", 0],
    [" 3 ", 3],
    ["5\n", 5],
  ])("should read %j as %i", (variant, expected) => {
    expect(liveMinUsers(variant)).toBe(expected);
  });

  it.each(["abc", "-1", "1.5", "", "   ", "1e3", "5px"])("should fall back to 0 for the unusable payload %j", (variant) => {
    expect(liveMinUsers(variant)).toBe(0);
  });

  it.each([null, undefined])("should fall back to 0 without a variant (%s)", (variant) => {
    expect(liveMinUsers(variant)).toBe(0);
  });

  it("should take the variant while its flag is on", () => {
    const on = vi.fn(() => true);
    const variant = vi.fn(() => "5");
    expect(liveMinUsersFrom(on, variant)).toBe(5);
    expect(on).toHaveBeenCalledWith("places-live-min-user");
    expect(variant).toHaveBeenCalledWith("places-live-min-user");
  });

  it("should ignore the variant while its flag is off", () => {
    expect(liveMinUsersFrom(() => false, () => "5")).toBe(0);
  });

  it("should be 0 when the flag is on without a variant", () => {
    expect(liveMinUsersFrom(() => true, () => null)).toBe(0);
  });

  it("should let a nonzero head count drop quieter scenes from the rail", () => {
    const rows = [destination({ id: "busy", user_count: 6 }), destination({ id: "quiet", user_count: 2 })];
    expect(selectLiveRail(rows, liveMinUsersFrom(() => true, () => "5")).map((row) => row.id)).toEqual(["busy"]);
  });
});

describe("when crediting a place", () => {
  it("should prefer the deployer over the owner and ignore values that are not wallets", () => {
    expect(
      placeCreatorAddress({
        creator_address: "0x00000000000000000000000000000000000000BB",
        owner: "0x00000000000000000000000000000000000000aa",
      }),
    ).toBe("0x00000000000000000000000000000000000000bb");
    expect(placeCreatorAddress({ creator_address: "Decentraland", owner: null })).toBeUndefined();
  });
});

describe("when reading creator profiles", () => {
  it("should post every address once and key the faces by lowercased address", async () => {
    const fetchMock = vi.fn(async () =>
      Response.json([
        { avatars: [{ ethAddress: "0xAA", name: "Ann", avatar: { snapshots: { face256: "https://cdn.example/face.png" } } }] },
      ]),
    );
    vi.stubGlobal("fetch", fetchMock);
    const result = await fetchPlaceCreators(["0xaa"]);
    expect(JSON.parse(String((fetchMock.mock.calls[0] as unknown as [string, RequestInit])[1].body))).toEqual({ ids: ["0xaa"] });
    expect(result["0xaa"]?.name).toBe("Ann");
  });

  it("should not call the network without addresses", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    await expect(fetchPlaceCreators([])).resolves.toEqual({});
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("should release the body of a failed read and reject so the cards keep their initials", async () => {
    const failed = new Response("upstream error", { status: 502 });
    const cancel = vi.spyOn(failed.body!, "cancel");
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => failed),
    );
    await expect(fetchPlaceCreators(["0xaa"])).rejects.toThrow("502");
    expect(cancel).toHaveBeenCalledOnce();
  });
});

describe("when the agent markdown reads a dehydrated page", () => {
  it("should return the first browse page", () => {
    const client = new QueryClient();
    client.setQueryData(placesKeys.browse({ category: "all", search: "" }), { pages: [page([destination()])], pageParams: [0] });
    expect(placesFromDehydrated(dehydrate(client)).map((row) => row.id)).toEqual(["plaza"]);
  });

  it("should return nothing when the page carried no browse query", () => {
    expect(placesFromDehydrated(undefined)).toEqual([]);
  });
});
