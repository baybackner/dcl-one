// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, within } from "@testing-library/react";
import { createRoutesStub } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import layout from "@/features/components/chrome/SitesLayout.module.css";

import CreateRoute from "./create._index";

vi.hoisted(() =>
  Object.assign(window, {
    __vite_plugin_react_preamble_installed__: true,
    $RefreshReg$: () => {},
    $RefreshSig$: () => (type: unknown) => type,
  }),
);

vi.mock("@/features/create-landing/CreateLanding", () => ({ default: () => <h1>Create landing</h1> }));
vi.mock("@/features/creators/components/SubNav", () => ({ SubNav: () => <nav aria-label="Creators sections" /> }));
vi.mock("@/data/lib/shop/marketplace-access", () => ({ useMarketplaceHere: () => false }));

afterEach(() => cleanup());

describe("when a visitor opens /create", () => {
  beforeEach(async () => {
    const Stub = createRoutesStub([
      { path: "/create", Component: CreateRoute, loader: () => ({ dehydratedState: undefined, downloads: null, sid: "s1" }) },
    ]);
    render(
      <QueryClientProvider client={new QueryClient()}>
        <Stub initialEntries={["/create"]} />
      </QueryClientProvider>,
    );
    await screen.findByRole("heading", { name: "Create landing" });
  });

  it("should show the Sites top bar", () => {
    const banner = within(screen.getByRole("banner", { name: "Decentraland" }));
    expect(banner.getByRole("button", { name: "Open menu" })).toBeTruthy();
    expect(banner.queryByRole("button", { name: "Toggle navigation menu" })).toBeNull();
  });

  it("should float the top bar over the landing's hero", () => {
    expect(document.querySelector("[data-site-layout]")?.classList.contains(layout.overlay)).toBe(true);
  });

  it("should render the landing and the site footer", () => {
    expect(screen.getByRole("heading", { name: "Create landing" })).toBeTruthy();
    expect(screen.getByRole("contentinfo")).toBeTruthy();
  });
});
