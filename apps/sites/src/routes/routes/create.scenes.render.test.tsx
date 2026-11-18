// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import CreateScenes from "./create.scenes";

vi.hoisted(() =>
  Object.assign(window, {
    __vite_plugin_react_preamble_installed__: true,
    $RefreshReg$: () => {},
    $RefreshSig$: () => (type: unknown) => type,
  }),
);
vi.mock("@/core/lib/telemetry/track", () => ({ track: vi.fn() }));
vi.mock("@/data/lib/auth/index", () => ({ useAuth: () => ({ isConnected: true, address: "0xabc" }) }));

afterEach(() => cleanup());

const PUBLISHED = {
  entityId: "baf-land",
  kind: "land",
  title: "Plaza",
  baseParcel: "1,1",
  pointers: ["1,1"],
  worldName: null,
  thumbnailUrl: null,
  deployedAt: 1,
};

describe("/create/scenes", () => {
  describe("when the creator has a published scene", () => {
    it("should offer its files for download and no editor, republish or template action", () => {
      const router = createMemoryRouter([{ id: "scenes", path: "/create/scenes", Component: CreateScenes as never }], {
        initialEntries: ["/create/scenes"],
        hydrationData: { loaderData: { scenes: { sid: "sid-example", publishedAddress: "0xabc", published: [PUBLISHED] } } },
      });

      render(<RouterProvider router={router} />);

      expect(screen.getByRole("button", { name: "Download the deployed files of Plaza" })).toBeTruthy();
      expect(screen.queryByRole("button", { name: /Open .* in the editor|Republish|Start from a template/ })).toBeNull();
      expect(screen.queryByRole("navigation", { name: "Scene lists" })).toBeNull();
    });
  });
});
