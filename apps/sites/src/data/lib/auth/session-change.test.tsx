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

import {
  AUTH_CALLBACK_PATH,
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

  it("does not mark a post that never reached the server", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => { throw new TypeError("Failed to fetch"); }));
    expect(await postAuthCallback({ signout: true }, { keepalive: true, revalidate: true })).toBeNull();
    expect(sessionChangePending()).toBe(false);
  });

  it("reloads root's data in place for a change that asks to, then holds no change", async () => {
    let reads = 0;
    const router = createMemoryRouter(
      [
        {
          id: "root",
          path: "/",
          loader: () => ({ read: ++reads }),
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
    await view.findByText("mounted");
    expect(reads).toBe(1);

    act(() => markSessionChanged({ revalidate: true }));
    await waitFor(() => expect(reads).toBe(2));
    await waitFor(() => expect(sessionChangePending()).toBe(false));

    // A change without `revalidate` waits for the next navigation.
    act(() => markSessionChanged());
    await act(async () => {});
    expect(reads).toBe(2);
    expect(sessionChangePending()).toBe(true);
  });
});
