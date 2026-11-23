// @vitest-environment jsdom
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { StrictMode, useSyncExternalStore } from "react";
import { createMemoryRouter, Outlet, RouterProvider } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() =>
  Object.assign(window, {
    __vite_plugin_react_preamble_installed__: true,
    $RefreshReg$: () => {},
    $RefreshSig$: () => (type: unknown) => type,
  }),
);

const page = vi.hoisted(() => vi.fn());
vi.mock("./track", () => ({ page }));

import { isOutsideLegacySites, isPageTrackingExempt, useNotFoundPageView, usePageView, useRoutePageView, type PageView } from "./page-view";

// A view a test sets once its "data" arrives, as a page reading it from a query does.
let pendingView: PageView | null = null;
const listeners = new Set<() => void>();
function setPendingView(view: PageView | null) {
  pendingView = view;
  listeners.forEach((l) => l());
}
function usePendingView() {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => pendingView,
  );
}

function Root() {
  useRoutePageView();
  return <Outlet />;
}
function BlogHome() {
  usePageView({ name: "Blog", properties: { title: "Blog", path: "/blog" } });
  return null;
}
function LoadingCommunity() {
  usePageView(usePendingView());
  return null;
}

function setup(session: { sid?: string | null; wallet?: string | null } = { sid: "sid-1" }, initial = "/", strict = false) {
  const router = createMemoryRouter(
    [
      {
        id: "root",
        path: "/",
        loader: () => session,
        Component: Root,
        HydrateFallback: () => null,
        children: [
          { index: true, Component: () => null },
          { path: "blog", Component: BlogHome },
          { path: "social/communities/:id", Component: LoadingCommunity },
          { path: "*", Component: () => null },
        ],
      },
    ],
    { initialEntries: [initial] },
  );
  const tree = <RouterProvider router={router} />;
  render(strict ? <StrictMode>{tree}</StrictMode> : tree);
  return router;
}

const names = () => page.mock.calls.map(([name]) => name);

describe("usePageView", () => {
  beforeEach(() => {
    page.mockReset();
    setPendingView(null);
  });
  afterEach(cleanup);

  it("sends page(pathname) on the first load with the session id and the verified wallet as userId", async () => {
    setup({ sid: "sid-1", wallet: "0xABC" }, "/create");
    await waitFor(() => expect(page).toHaveBeenCalledTimes(1));
    expect(page).toHaveBeenCalledWith("/create", {}, { sid: "sid-1", userId: "0xABC" });
  });

  it("sends no userId without a verified wallet", async () => {
    setup({ sid: "sid-1", wallet: null }, "/create");
    await waitFor(() => expect(page).toHaveBeenCalledTimes(1));
    expect(page.mock.calls[0]?.[2]).toEqual({ sid: "sid-1" });
  });

  it("sends again on a pathname change, not on a search-only change", async () => {
    const router = setup(undefined, "/create");
    await waitFor(() => expect(page).toHaveBeenCalledTimes(1));
    await act(() => router.navigate("/create?utm_source=x"));
    await act(() => router.navigate("/events"));
    await waitFor(() => expect(names()).toEqual(["/create", "/events"]));
    await act(() => router.navigate("/events?tab=live"));
    await act(() => router.navigate("/create"));
    await waitFor(() => expect(names()).toEqual(["/create", "/events", "/create"]));
  });

  it("sends once per navigation under StrictMode", async () => {
    const router = setup(undefined, "/help", true);
    await waitFor(() => expect(page).toHaveBeenCalledTimes(1));
    await act(() => router.navigate("/invite/someone"));
    await waitFor(() => expect(names()).toEqual(["/help", "/invite/someone"]));
  });

  it("sends nothing on the suppressed paths, and does on /events", async () => {
    const router = setup(undefined, "/whats-on");
    for (const path of ["/discover/places", "/events/event", "/cast", "/storage/env", "/places/abc", "/social/communities/1"]) {
      await act(() => router.navigate(path));
    }
    await act(() => router.navigate("/events"));
    await waitFor(() => expect(names()).toEqual(["/events"]));
  });

  it("sends nothing on surfaces the legacy site never served, the Shop included", async () => {
    const router = setup(undefined, "/shop/items");
    for (const path of ["/marketplace/browse", "/builder/collections", "/play", "/auth/callback", "/creator-hub/x"]) {
      await act(() => router.navigate(path));
    }
    await act(() => router.navigate("/create"));
    await waitFor(() => expect(names()).toEqual(["/create"]));
  });

  it("lets a route's own page view replace the default for that navigation", async () => {
    const router = setup(undefined, "/create");
    await waitFor(() => expect(page).toHaveBeenCalledTimes(1));
    await act(() => router.navigate("/blog"));
    await waitFor(() => expect(page).toHaveBeenCalledTimes(2));
    expect(page.mock.calls[1]).toEqual(["Blog", { title: "Blog", path: "/blog" }, { sid: "sid-1" }]);
    await act(() => router.navigate("/blog?page=2"));
    expect(page).toHaveBeenCalledTimes(2);
  });

  it("waits for a named page's data, then sends it once, and again when its properties change", async () => {
    setup(undefined, "/social/communities/c1");
    await act(async () => {});
    expect(page).not.toHaveBeenCalled();
    act(() => setPendingView({ name: "Builders", properties: { communityId: "c1", privacy: "public", membersCount: 3 } }));
    await waitFor(() => expect(page).toHaveBeenCalledTimes(1));
    expect(page.mock.calls[0]?.[0]).toBe("Builders");
    act(() => setPendingView({ name: "Builders", properties: { communityId: "c1", privacy: "public", membersCount: 3 } }));
    expect(page).toHaveBeenCalledTimes(1);
    act(() => setPendingView({ name: "Builders", properties: { communityId: "c1", privacy: "public", membersCount: 4 } }));
    await waitFor(() => expect(page).toHaveBeenCalledTimes(2));
  });

  it("sends nothing without a session id", async () => {
    const router = setup({ sid: null }, "/create");
    await act(() => router.navigate("/blog"));
    await act(async () => {});
    expect(page).not.toHaveBeenCalled();
  });

});

describe("useNotFoundPageView", () => {
  beforeEach(() => page.mockReset());
  afterEach(cleanup);

  function NotFoundAt({ notFound }: { notFound: boolean }) {
    useNotFoundPageView(notFound);
    return null;
  }

  function renderAt(path: string, notFound: boolean) {
    const router = createMemoryRouter(
      [{ id: "root", path: "*", loader: () => ({ sid: "sid-1" }), Component: () => <NotFoundAt notFound={notFound} />, HydrateFallback: () => null }],
      { initialEntries: [path] },
    );
    render(<RouterProvider router={router} />);
  }

  // The root error boundary renders it in place of the app, so the default page(pathname) is not sent with it.
  it("sends the 404's own page view with its path", async () => {
    renderAt("/missing/thing", true);
    await waitFor(() => expect(page).toHaveBeenCalledTimes(1));
    expect(page).toHaveBeenCalledWith("Not Found", { path: "/missing/thing" }, { sid: "sid-1" });
  });

  it("sends nothing for another error status", async () => {
    renderAt("/nope", false);
    await act(async () => {});
    expect(page).not.toHaveBeenCalled();
  });

  it("sends nothing for a missing page under a path whose pages own their view", async () => {
    renderAt("/places/unknown/deep", true);
    await act(async () => {});
    expect(page).not.toHaveBeenCalled();
  });
});

describe("isOutsideLegacySites", () => {
  it.each(["/shop", "/shop/items", "/shop-native", "/marketplace", "/builder/x", "/play", "/auth/callback", "/internal/client-error", "/api/x"])(
    "excludes %s",
    (path) => expect(isOutsideLegacySites(path)).toBe(true),
  );

  it.each(["/", "/shopping", "/create", "/help", "/download", "/events", "/players"])(
    "keeps %s",
    (path) => expect(isOutsideLegacySites(path)).toBe(false),
  );
});

describe("isPageTrackingExempt", () => {
  it.each(["/blog", "/blog/news", "/whats-on", "/whats-on/1", "/discover", "/discover/x", "/events/event", "/cast", "/cast/s/k", "/storage", "/storage/env", "/social", "/social/communities/1", "/places", "/places/world/x"])(
    "suppresses %s",
    (path) => expect(isPageTrackingExempt(path)).toBe(true),
  );

  it.each(["/", "/events", "/events/event/1", "/blogs", "/placesx", "/create", "/download", "/account/confirm-email/abc"])(
    "keeps %s",
    (path) => expect(isPageTrackingExempt(path)).toBe(false),
  );
});
