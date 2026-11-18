// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import CreateHome from "./create.home";

vi.hoisted(() =>
  Object.assign(window, {
    __vite_plugin_react_preamble_installed__: true,
    $RefreshReg$: () => {},
    $RefreshSig$: () => (type: unknown) => type,
  }),
);
vi.mock("@/core/lib/telemetry/track", () => ({ track: vi.fn() }));
vi.mock("@/data/lib/auth/index", () => ({ useAuth: () => ({ isConnected: false, address: undefined }) }));

afterEach(() => cleanup());

const SCENE = { id: "s-1", title: "Genesis Plaza", base_position: "0,0", world_name: null };

function renderHome() {
  const router = createMemoryRouter(
    [
      { id: "home", path: "/create/home", Component: CreateHome as never },
      { path: "/download/creator-hub", element: <h1>Creator Hub download</h1> },
    ],
    {
      initialEntries: ["/create/home"],
      hydrationData: {
        loaderData: {
          home: { sid: "sid-example", creator: "", profileAddress: "", profileName: "", scenes: [SCENE], scenesError: false },
        },
      },
    },
  );
  render(<RouterProvider router={router} />);
}

describe("/create/home", () => {
  describe("when the creator has published scenes", () => {
    it("should list them without a link into a scene editor or a template strip", () => {
      renderHome();

      expect(screen.getByRole("link", { name: "Genesis Plaza" }).getAttribute("href")).toBe("/create/scenes");
      expect(screen.queryByRole("region", { name: "Start from a template" })).toBeNull();
      const hrefs = screen.getAllByRole("link").map((link) => link.getAttribute("href") ?? "");
      expect(hrefs.filter((href) => /^\/create\/(scene-editor|templates|publish)\b/.test(href))).toEqual([]);
    });
  });

  describe("and a signed-out visitor asks to start building", () => {
    it("should send them to the desktop Creator Hub download", async () => {
      renderHome();

      await userEvent.click(screen.getByRole("button", { name: "Start building" }));

      expect(await screen.findByRole("heading", { name: "Creator Hub download" })).toBeTruthy();
    });
  });
});
