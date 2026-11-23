// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

const facts = vi.hoisted(() => {
  Object.assign(window, { __vite_plugin_react_preamble_installed__: true });
  return { identity: null as { signer: string } | null, openSignIn: vi.fn() };
});
vi.mock("@/data/lib/auth/context", () => ({ useAuth: () => ({ identity: facts.identity }) }));
vi.mock("@/core/lib/telemetry/track", () => ({ track: vi.fn() }));
vi.mock("../../components/auth/signin-store", () => ({ openSignIn: facts.openSignIn }));
vi.mock("../../components/chrome/SitesLayout", () => ({ default: ({ children }: { children: import("react").ReactNode }) => <>{children}</> }));
vi.mock("@/data/lib/catalyst/cast", async (original) => ({
  ...(await original<typeof import("@/data/lib/catalyst/cast")>()),
  getWatcherToken: async () => ({ url: "wss://livekit.test", token: "jwt", roomId: "room-1", identity: "Ada", placeName: "Cast Platforms Plaza" }),
}));
vi.mock("./session", () => ({
  joinCast: async () => ({ tracks: () => [], participantCount: () => 1, onAir: () => true, reconnecting: () => null, disconnected: () => false, subscribe: () => () => {}, canPlaybackAudio: () => true, disconnect: async () => {} }),
}));

import LiveCastWatch from "./LiveCastWatch";

const WORLD = "stage.dcl.eth";
const LOBBY = { name: `Decentraland Cast from ${WORLD}` };

function open() {
  const Page = () => <LiveCastWatch trackCtx={{ sid: "sid", story: "cast/watch", variant: "live" }} location={WORLD} />;
  render(<RouterProvider router={createMemoryRouter([{ path: "/", Component: Page }])} />);
}

afterEach(() => {
  cleanup();
  facts.identity = null;
  facts.openSignIn.mockClear();
});

describe("the Cast watcher page", () => {
  it("names the place the same way before the join and after LEAVE", async () => {
    facts.identity = { signer: "0x" + "ab".repeat(20) };
    open();
    expect(await screen.findByRole("heading", LOBBY)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "JOIN NOW" }));
    fireEvent.click(await screen.findByRole("button", { name: "Leave" }));
    expect(await screen.findByRole("heading", LOBBY)).toBeTruthy();
  });
});
