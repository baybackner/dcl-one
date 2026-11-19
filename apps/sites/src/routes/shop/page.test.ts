// @vitest-environment jsdom
import { act, createElement, type ReactNode } from "react";
import { hydrateRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { REMOTE_FLAG, primeRemoteFlags, useRemoteFlag } from "@dcl/one-ui/apps/shop/remote-flags";
import { shopFlags } from "@ui/shop/discovery/flags";

import { defaultShopSeo, type ShopPageData } from "@/data/lib/shop/pages";

import ShopLayout from "../routes/shop";
import { shopClientLoader } from "./page";

vi.hoisted(() => {
  Object.assign(window, {
    __vite_plugin_react_preamble_installed__: true,
    $RefreshReg$: () => {},
    $RefreshSig$: () => (type: unknown) => type,
  });
});

vi.mock("./host", () => ({ default: ({ children }: { children: ReactNode }) => children }));

afterEach(() => {
  primeRemoteFlags(null);
  vi.unstubAllGlobals();
});

function FlagProbe() {
  return useRemoteFlag(REMOTE_FLAG.creatorSales) ? "Deals available" : "Deals unavailable";
}

function shopPage(loaderData: ShopPageData) {
  const router = createMemoryRouter(
    [{ path: "/shop", Component: ShopLayout, children: [{ id: "shopPage", path: "overview", Component: FlagProbe, loader: () => loaderData }] }],
    { initialEntries: ["/shop/overview"], hydrationData: { loaderData: { shopPage: loaderData } } },
  );
  return createElement(RouterProvider, { router });
}

describe("a Shop page's server data", () => {
  it("uses the loader answer on first paint and seeds the browser store after hydration", async () => {
    const data: ShopPageData = {
      page: "overview",
      seo: defaultShopSeo("overview"),
      queries: [],
      featureFlags: { [`dapps-${REMOTE_FLAG.creatorSales}`]: true },
    };
    const container = document.createElement("div");
    container.innerHTML = renderToString(shopPage(data));
    expect(container.textContent).toBe("Deals available");
    expect(shopFlags().creatorSales).toBe(false);

    const onRecoverableError = vi.fn();
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    const root = hydrateRoot(container, shopPage(data), { onRecoverableError });
    try {
      await act(async () => {});
      expect(container.textContent).toBe("Deals available");
      expect(onRecoverableError).not.toHaveBeenCalled();
      expect(shopFlags().creatorSales).toBe(true);
    } finally {
      await act(() => root.unmount());
    }
  });

  it("primes the flags for a Shop reached by a client navigation, before the page renders", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ [`dapps-${REMOTE_FLAG.creatorSales}`]: true }))));
    expect(shopFlags().creatorSales).toBe(false);
    expect(await shopClientLoader()).toBeNull();
    expect(shopFlags().creatorSales).toBe(true);
  });
});
