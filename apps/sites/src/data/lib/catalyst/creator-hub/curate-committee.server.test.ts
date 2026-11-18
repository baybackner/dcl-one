import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../client")>();
  return { ...actual, getJSON: vi.fn() };
});

import { getJSON } from "../client";
import { loadCommitteeCuration } from "./curate-committee.server";

const OWNER = "0x1111111111111111111111111111111111111111";

afterEach(() => {
  vi.resetAllMocks();
});

describe("loadCommitteeCuration", () => {
  describe("when the node lists a collection under review", () => {
    it("should carry the node's ISO-8601 timestamps onto the row and its curation", async () => {
      vi.mocked(getJSON).mockResolvedValueOnce({
        ok: true,
        data: {
          committee: [],
          collections: [
            {
              id: "c1",
              name: "Hats",
              type: "standard",
              is_programmatic: false,
              status: "under_review",
              is_approved: false,
              has_reviews: true,
              item_count: 2,
              owner: OWNER,
              created_at: "2023-12-01T00:00:00.000Z",
              reviewed_at: null,
              curation: {
                id: "cur-1",
                collection_id: "c1",
                assignee: OWNER,
                status: "rejected",
                created_at: "2024-01-01T00:00:00.000Z",
                updated_at: null,
              },
            },
          ],
        },
      });
      const { allRows, error } = await loadCommitteeCuration({ isCommittee: true });
      expect(error).toBe(false);
      expect(allRows[0]).toMatchObject({
        createdAt: "2023-12-01T00:00:00.000Z",
        curation: { created_at: "2024-01-01T00:00:00.000Z", updated_at: "1970-01-01T00:00:00.000Z" },
      });
    });
  });
});
