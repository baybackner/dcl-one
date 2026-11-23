// @vitest-environment jsdom
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { createMemoryRouter, Outlet, RouterProvider } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() =>
  Object.assign(window, {
    __vite_plugin_react_preamble_installed__: true,
    $RefreshReg$: () => {},
    $RefreshSig$: () => (type: unknown) => type,
  }),
);

import { restoreTrackedUser, trackedUserDropped } from "@/core/lib/telemetry/tracked-user";

import {
  AUTH_CALLBACK_PATH,
  beginSignOut,
  markSessionChanged,
  postAuthCallback,
  sessionChangeApplied,
  sessionChangePending,
  useSessionChangeRevalidation,
} from "./session-change";

function Root() {
  useSessionChangeRevalidation();
  return (
    <>
      <p>mounted</p>
      <Outlet />
    </>
  );
}

describe("session change", () => {
  beforeEach(() => {
    restoreTrackedUser();
    if (sessionChangePending()) sessionChangeApplied();
  });
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("marks the change for any answer of /auth/callback, a refusal included (it clears the cookie)", async () => {
    const fetchMock = vi.fn(async () => Response.json({ ok: false }, { status: 401 }));
    vi.stubGlobal("fetch", fetchMock);
    const response = await postAuthCallback({ token: "expired" });
    expect(response?.status).toBe(401);
    expect(fetchMock).toHaveBeenCalledWith(AUTH_CALLBACK_PATH, expect.objectContaining({ method: "POST", body: '{"token":"expired"}' }));
    expect(sessionChangePending()).toBe(true);
  });

  it("lets only a successful sign-in restore an account a sign-out left out", async () => {
    beginSignOut();
    vi.stubGlobal("fetch", vi.fn(async () => new Response("error", { status: 500 })));
    await postAuthCallback({ token: "t" });
    expect(trackedUserDropped()).toBe(true);
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ ok: false }, { status: 401 })));
    await postAuthCallback({ token: "expired" });
    expect(trackedUserDropped()).toBe(true);
    vi.stubGlobal("fetch", vi.fn(async () => Response.json({ ok: true })));
    await postAuthCallback({ token: "good" });
    expect(trackedUserDropped()).toBe(false);
  });

  it("does not mark a post that never reached the server", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => { throw new TypeError("Failed to fetch"); }));
    expect(await postAuthCallback({ signout: true }, { keepalive: true, revalidate: true })).toBeNull();
    expect(sessionChangePending()).toBe(false);
  });

  function renderRoot() {
    const loads = { count: 0 };
    const router = createMemoryRouter(
      [
        {
          id: "root",
          path: "/",
          loader: () => ({ read: ++loads.count }),
          shouldRevalidate: ({ currentUrl, nextUrl, defaultShouldRevalidate }) =>
            sessionChangePending() || (currentUrl.pathname !== nextUrl.pathname && defaultShouldRevalidate),
          Component: Root,
          HydrateFallback: () => null,
          children: [{ path: "*", Component: () => null }],
        },
      ],
      { initialEntries: ["/events"] },
    );
    const view = render(<RouterProvider router={router} />);
    return { loads, view, router };
  }

  it("reloads root's data in place for a change that asks to, then holds no change", async () => {
    const { loads, view, router } = renderRoot();
    await view.findByText("mounted");
    expect(loads.count).toBe(1);

    act(() => markSessionChanged({ revalidate: true }));
    await waitFor(() => expect(loads.count).toBe(2));
    await waitFor(() => expect(sessionChangePending()).toBe(false));
    router.dispose();
  });

  it("keeps a change without `revalidate` for the next navigation", async () => {
    const { loads, view, router } = renderRoot();
    await view.findByText("mounted");

    act(() => markSessionChanged());
    await act(async () => {});
    expect(loads.count).toBe(1);
    expect(sessionChangePending()).toBe(true);

    await act(() => router.navigate("/places"));
    await waitFor(() => expect(loads.count).toBe(2));
    await waitFor(() => expect(sessionChangePending()).toBe(false));
    router.dispose();
  });

  it("runs a reload asked for before the router mounted once it mounts", async () => {
    markSessionChanged({ revalidate: true });
    const { loads, router } = renderRoot();
    await waitFor(() => expect(loads.count).toBe(2));
    await waitFor(() => expect(sessionChangePending()).toBe(false));
    router.dispose();
  });

  it("does not reload on mount for a change before it that did not ask to", async () => {
    markSessionChanged();
    const { loads, view, router } = renderRoot();
    await view.findByText("mounted");
    await act(async () => {});
    expect(loads.count).toBe(1);
    expect(sessionChangePending()).toBe(true);
    router.dispose();
  });
});
