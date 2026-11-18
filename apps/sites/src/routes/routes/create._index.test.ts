import { RouterContextProvider } from "react-router";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { loader } from "./create._index";

vi.mock("@/data/lib/create-landing/live-scenes", () => ({
  liveScenesQuery: () => ({ queryKey: ["live-scenes"], queryFn: async () => [] }),
}));
vi.mock("@/data/lib/create-landing/blog-posts.server", () => ({ latestPosts: vi.fn(async () => []) }));
vi.mock("@/core/lib/telemetry/page-loader", () => ({
  sidLoader: () => ({ wrap: (payload: unknown) => payload }),
}));

beforeEach(() => vi.clearAllMocks());

describe("/create", () => {
  describe("when the link carries a projectUrl", () => {
    it("should render the landing instead of redirecting into a scene editor", async () => {
      const request = new Request("https://sites.test/create?projectUrl=http%3A%2F%2F127.0.0.1%3A8000%2F");

      const payload = await loader({ request, context: new RouterContextProvider() } as never);

      expect(payload).toHaveProperty("dehydratedState");
    });
  });
});
