import { RouterContextProvider } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fetchEventCategories } from "@/data/lib/catalyst/places/events";
import { loader as editLoader } from "./events_.edit-event.$id";
import { loader as newLoader } from "./events_.new-event";

vi.mock("@/data/lib/catalyst/places/events", async (original) => ({
  ...(await original<typeof import("@/data/lib/catalyst/places/events")>()),
  fetchEventCategories: vi.fn(),
}));

const EVENT_ID = "5f0c6a52-6b1d-4c1e-9a77-0e8f5b2c4d10";

const args = (path: string, params: Record<string, string> = {}) =>
  ({ request: new Request(`https://sites.test${path}`), params, context: new RouterContextProvider() }) as never;

describe("when the hangout editor loads", () => {
  beforeEach(() => {
    vi.mocked(fetchEventCategories).mockResolvedValue([]);
  });
  afterEach(() => vi.resetAllMocks());

  describe("and the visitor opens /events/new-event", () => {
    it("should open the editor in create mode with the community from the query", async () => {
      const { data } = await newLoader(args("/events/new-event?community_id=c-1"));
      expect(data).toMatchObject({ mode: "create", eventId: null, communityId: "c-1" });
    });

    it("should ignore an id in the query", async () => {
      const { data } = await newLoader(args("/events/new-event?mode=edit&id=ev-1"));
      expect(data).toMatchObject({ mode: "create", eventId: null });
    });
  });

  describe("and the visitor opens /events/edit-event/:id", () => {
    it("should open the editor in edit mode for the event in the path", async () => {
      const { data } = await editLoader(args(`/events/edit-event/${EVENT_ID}?community_id=c-1`, { id: EVENT_ID }));
      expect(data).toMatchObject({ mode: "edit", eventId: EVENT_ID, communityId: "" });
    });

    it("should treat an id that is not a UUID as no event, without asking the API", async () => {
      const { data } = await editLoader(args("/events/edit-event/not-an-id", { id: "not-an-id" }));
      expect(data).toMatchObject({ mode: "edit", eventId: null });
    });

    it("should ask for an event when the path id is blank", async () => {
      const { data } = await editLoader(args("/events/edit-event/%20", { id: " " }));
      expect(data).toMatchObject({ mode: "edit", eventId: null });
    });
  });

  describe("and a category has multiple translations", () => {
    beforeEach(() => {
      vi.mocked(fetchEventCategories).mockResolvedValueOnce([
        { name: "music", active: true, i18n: { en: "Music", es: "Música" } },
      ]);
    });

    it("should retain the translations for the locale selected on the client", async () => {
      const { data } = await newLoader(args("/events/new-event"));
      expect(data.categories).toEqual([
        { name: "music", label: "Music", i18n: { en: "Music", es: "Música" } },
      ]);
    });
  });

  describe("and the categories cannot be read", () => {
    beforeEach(() => {
      vi.mocked(fetchEventCategories).mockRejectedValue(new Error("down"));
    });

    it("should still open the editor and flag the categories", async () => {
      const { data } = await newLoader(args("/events/new-event"));
      expect(data).toMatchObject({ mode: "create", categories: [], categoriesError: true });
    });
  });
});
