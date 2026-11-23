// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  Object.assign(window, { __vite_plugin_react_preamble_installed__: true });
});
vi.mock("../../components/chrome/SitesLayout", () => ({ default: ({ children }: { children: import("react").ReactNode }) => <>{children}</> }));

import CastWatch, { type WatchLive } from "./CastWatch";

const WATCH = { status: "live", location: "0,0", isWorld: false, placeName: "Test Plaza", identity: "Guest#1" } as const;

/** The page after a click on JOIN NOW. */
async function joined(live: Partial<WatchLive>) {
  const all: WatchLive = {
    join: async () => ({ roomId: "room-1" }),
    leave: vi.fn(),
    stage: () => <video />,
    participantCount: 1,
    signal: "live",
    bindLost: () => () => {},
    audioBlocked: false,
    enableAudio: vi.fn(),
    ...live,
  };
  const Page = () => (
    <CastWatch trackCtx={{ sid: "sid", story: "cast/watch", variant: "live" }} watch={WATCH} live={all} onTrack={vi.fn()} />
  );
  const view = render(<RouterProvider router={createMemoryRouter([{ path: "/", Component: Page }])} />);
  fireEvent.click(await screen.findByRole("button", { name: "JOIN NOW" }));
  return view.container;
}

const people = () => screen.getByRole("button", { name: "Participants" }).textContent;

afterEach(cleanup);

describe("the Cast watcher page", () => {
  it("shows LIVE and the stage while a track is on air", async () => {
    const container = await joined({ participantCount: 2 });
    expect(await screen.findByText("LIVE")).toBeTruthy();
    expect(container.querySelector("video")).not.toBeNull();
    expect(people()).toBe("2");
  });

  it("says no one is casting, without LIVE, in a room where nothing is on air", async () => {
    const container = await joined({ signal: "none" });
    expect(await screen.findByText("No one is currently casting to this link...")).toBeTruthy();
    expect(screen.queryByText("LIVE")).toBeNull();
    expect(container.querySelector(".cast-watch")?.getAttribute("data-state")).toBe("waiting");
    expect(people()).toBe("1");
  });

  it("shows no people number when no room was joined", async () => {
    await joined({
      participantCount: 0,
      join: async () => ({ refused: { kind: "waiting", title: "No stream", message: "Soon." } }),
    });
    await screen.findByText("Soon.");
    expect(people()).toBe("");
  });

  it("names a join that failed, and Retry joins again", async () => {
    const join = vi
      .fn<WatchLive["join"]>(async () => ({ roomId: "room-1" }))
      .mockRejectedValueOnce(new Error("The Cast server did not answer in time"));
    await joined({ join });
    expect(await screen.findByText("The Cast server did not answer in time")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("LIVE")).toBeTruthy();
    expect(join).toHaveBeenCalledTimes(2);
  });

  it("says it reconnects, without LIVE, while the Cast does not arrive", async () => {
    const container = await joined({ signal: "reconnecting" });
    expect((await screen.findByRole("status")).textContent).toContain("Reconnecting");
    expect(screen.queryByText("LIVE")).toBeNull();
    expect(container.querySelector("video")).not.toBeNull();
    expect(container.querySelector(".cast-watch")?.getAttribute("data-state")).toBe("reconnecting");
  });

  it("joins again by itself when the room is lost", async () => {
    let lost = () => {};
    const join = vi.fn<WatchLive["join"]>(async () => ({ roomId: "room-1" }));
    await joined({ join, bindLost: (onLost) => ((lost = onLost), () => {}) });
    await screen.findByText("LIVE");
    act(() => lost());
    await waitFor(() => expect(join).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("LIVE")).toBeTruthy();
  });
});
