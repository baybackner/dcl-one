// @vitest-environment jsdom
import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AuthIdentity } from "@/data/lib/auth/types";
import { CatalystError, postJSON, signedGetJSON } from "@/data/lib/catalyst/client";
import { eventsPageKeys } from "@/data/lib/catalyst/places/events-page";
import { openSignIn } from "../../components/auth/signin-store";
import { fakeEventsApi, makeEvent, makeIdentity, readsOf, renderAdminPage, type FakeEventsApi } from "./admin.test-utils";
import PendingEventsPage from "./PendingEventsPage";

const auth = vi.hoisted(() => ({ identity: null as AuthIdentity | null, isConnected: false }));
vi.hoisted(() => { Object.assign(window, { __vite_plugin_react_preamble_installed__: true }); });
vi.mock("@/data/lib/auth/context", () => ({ useAuth: () => auth }));
vi.mock("@/data/lib/auth/session", () => ({ getIdentity: () => auth.identity }));
vi.mock("../../components/auth/signin-store", () => ({ openSignIn: vi.fn() }));
vi.mock("../../components/chrome/SitesLayout", () => ({ default: ({ children }: { children: React.ReactNode }) => <>{children}</> }));
vi.mock("@/data/lib/catalyst/client", async (original) => ({
  ...await original<typeof import("@/data/lib/catalyst/client")>(),
  postJSON: vi.fn(), signedGetJSON: vi.fn(),
}));

const PATH = "/events/admin/pending-events";
const SETTINGS = "/events/api/profiles/me/settings";
const EVENTS = "/events/api/events";
const moderation = (id: string, body: unknown) => ["/events/api/events/" + id, body, expect.objectContaining({ method: "PATCH" })];

describe("when opening the Pending Events page", () => {
  let identity: AuthIdentity;
  let api: FakeEventsApi;
  let user: ReturnType<typeof userEvent.setup>;

  beforeEach(() => {
    identity = makeIdentity();
    auth.identity = identity;
    auth.isConnected = true;
    api = {
      profile: { user: identity.signer, permissions: ["approve_any_event"] },
      profiles: "forbidden",
      events: [makeEvent(), makeEvent({ id: "approved-one", name: "Sun party", approved: true })],
      failNext: {},
    };
    fakeEventsApi(api);
    user = userEvent.setup();
  });

  afterEach(() => {
    cleanup();
    vi.resetAllMocks();
  });

  describe("and the visitor is signed out", () => {
    beforeEach(() => {
      auth.identity = null;
      auth.isConnected = false;
    });

    it("should send the visitor to sign in without reading the queue", async () => {
      renderAdminPage(<PendingEventsPage />, PATH);
      expect(await screen.findByRole("button", { name: "Sign in" })).toBeTruthy();
      await waitFor(() => expect(openSignIn).toHaveBeenCalledWith({ redirectTo: PATH }));
      expect(signedGetJSON).not.toHaveBeenCalled();
    });
  });

  describe("and the account has no event permission", () => {
    beforeEach(() => { api.profile.permissions = []; });

    it("should redirect to /events without reading the queue", async () => {
      renderAdminPage(<PendingEventsPage />, PATH);
      await waitFor(() => expect(screen.getByTestId("location").textContent).toBe("/events"));
      expect(readsOf(EVENTS)).toBe(0);
    });
  });

  describe("and the account may only approve its own events", () => {
    beforeEach(() => {
      api.profile.permissions = ["approve_own_event"];
      api.events = [
        makeEvent({ id: "own-one", name: "My own show" }),
        makeEvent({ id: "other-one", name: "Someone else's show", user: "0x5555555555555555555555555555555555555555" }),
      ];
    });

    it("should list only its own pending event and let it approve that one", async () => {
      renderAdminPage(<PendingEventsPage />, PATH);
      const pending = await screen.findByRole("region", { name: "Pending Events" });
      await user.click(await within(pending).findByRole("link", { name: "My own show" }));
      expect(within(pending).queryByRole("link", { name: "Someone else's show" })).toBeNull();
      const review = screen.getByRole("dialog", { name: "My own show" });
      expect(within(review).queryByRole("button", { name: "Reject" })).toBeNull();
      await user.click(within(review).getByRole("button", { name: "Approve" }));
      expect(await screen.findByText("Event/Experience approved successfully")).toBeTruthy();
      expect(postJSON).toHaveBeenCalledWith(...moderation("own-one", { approved: true }));
    });

    it("should offer no approval for someone else's event opened by link", async () => {
      api.events[1] = { ...api.events[1]!, approved: true };
      renderAdminPage(<PendingEventsPage />, `${PATH}?id=other-one`);
      const review = await screen.findByRole("dialog", { name: "Someone else's show" });
      expect(within(review).queryByRole("button", { name: "Approve" })).toBeNull();
      expect(within(review).queryByRole("button", { name: "Edit Event" })).toBeNull();
    });
  });

  describe("and the permission read is refused", () => {
    beforeEach(() => { api.failNext.profile = new CatalystError("Forbidden", SETTINGS, 403); });

    it("should redirect to /events", async () => {
      renderAdminPage(<PendingEventsPage />, PATH);
      await waitFor(() => expect(screen.getByTestId("location").textContent).toBe("/events"));
    });
  });

  describe("and the permission read fails", () => {
    beforeEach(() => { api.failNext.profile = new Error("Settings unavailable"); });

    it("should offer a retry instead of redirecting, and load the page once it succeeds", async () => {
      renderAdminPage(<PendingEventsPage />, PATH);
      expect((await screen.findByRole("alert")).textContent).toBe("Settings unavailable");
      expect(screen.getByTestId("location").textContent).toBe(PATH);
      await user.click(screen.getByRole("button", { name: "Retry" }));
      expect(await screen.findByRole("link", { name: "Moon concert" })).toBeTruthy();
    });
  });

  it("should show the admin bar with Pending Events current and no Users tab for a non-moderator list", async () => {
    renderAdminPage(<PendingEventsPage />, PATH);
    const bar = await screen.findByRole("navigation", { name: "Event administration" });
    expect(within(bar).getByRole("link", { name: "Pending Events" }).getAttribute("aria-current")).toBe("page");
    expect(within(bar).getByRole("link", { name: "Events" }).getAttribute("href")).toBe("/events");
    expect(within(bar).queryByRole("link", { name: "Users" })).toBeNull();
    expect(within(bar).getByRole("link", { name: "+ Create Event" }).getAttribute("href")).toBe("/events/new-event");
  });

  it("should list the pending events and the recently approved ones with their status", async () => {
    renderAdminPage(<PendingEventsPage />, PATH);
    const pending = await screen.findByRole("region", { name: "Pending Events" });
    expect(await within(pending).findByRole("link", { name: "Moon concert" })).toBeTruthy();
    expect(within(pending).getByText("Pending")).toBeTruthy();
    expect(within(pending).getByText("Luna")).toBeTruthy();
    const recent = screen.getByRole("region", { name: "Recently Approved (Last 24hs)" });
    expect(within(recent).getByRole("link", { name: "Sun party" })).toBeTruthy();
    expect(within(recent).getByText("Approved")).toBeTruthy();
  });

  describe("and nothing waits for approval", () => {
    beforeEach(() => { api.events = []; });

    it("should say so", async () => {
      renderAdminPage(<PendingEventsPage />, PATH);
      expect(await screen.findByText("No events waiting for approval")).toBeTruthy();
    });
  });

  describe("and the queue fails to load", () => {
    beforeEach(() => { api.failNext.events = new Error("Queue unavailable"); });

    it("should show the failure and reload the queue on retry", async () => {
      renderAdminPage(<PendingEventsPage />, PATH);
      expect((await screen.findByRole("alert")).textContent).toBe("Queue unavailable");
      await user.click(screen.getByRole("button", { name: "Retry" }));
      expect(await screen.findByRole("link", { name: "Moon concert" })).toBeTruthy();
    });
  });

  it("should open the review from a card and approve the event", async () => {
    const { client } = renderAdminPage(<PendingEventsPage />, PATH);
    client.setQueryData(eventsPageKeys.detail("pending-one"), makeEvent());
    await user.click(await screen.findByRole("link", { name: "Moon concert" }));
    expect(screen.getByTestId("location").textContent).toBe(`${PATH}?id=pending-one`);
    const review = screen.getByRole("dialog", { name: "Moon concert" });
    expect(within(review).getByText("A concert")).toBeTruthy();
    await user.click(within(review).getByRole("button", { name: "Approve" }));
    expect(await screen.findByText("Event/Experience approved successfully")).toBeTruthy();
    expect(postJSON).toHaveBeenCalledWith(...moderation("pending-one", { approved: true }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(client.getQueryState(eventsPageKeys.detail("pending-one"))?.isInvalidated).toBe(true);
    await waitFor(() => expect(readsOf(EVENTS)).toBe(2));
  });

  it("should open the review a deep link names", async () => {
    renderAdminPage(<PendingEventsPage />, `${PATH}?id=pending-one`);
    expect(await screen.findByRole("dialog", { name: "Moon concert" })).toBeTruthy();
  });

  it("should offer no moderation for an event already approved", async () => {
    renderAdminPage(<PendingEventsPage />, PATH);
    await user.click(await screen.findByRole("link", { name: "Sun party" }));
    const review = screen.getByRole("dialog", { name: "Sun party" });
    expect(within(review).queryByRole("button", { name: "Approve" })).toBeNull();
    expect(within(review).queryByRole("button", { name: "Reject" })).toBeNull();
  });

  describe("and the account may edit any event", () => {
    beforeEach(() => { api.profile.permissions = ["edit_any_event"]; });

    it("should edit from the review but not approve", async () => {
      renderAdminPage(<PendingEventsPage />, PATH);
      await user.click(await screen.findByRole("link", { name: "Moon concert" }));
      const review = screen.getByRole("dialog", { name: "Moon concert" });
      expect(within(review).queryByRole("button", { name: "Approve" })).toBeNull();
      await user.click(within(review).getByRole("button", { name: "Edit Event" }));
      expect(screen.getByTestId("location").textContent).toBe("/events/edit-event/pending-one");
    });
  });

  describe("and the approval fails", () => {
    beforeEach(() => { api.failNext.moderate = new Error("The hangout changed. Refresh."); });

    it("should keep the review and the card, report the failure and allow another try", async () => {
      renderAdminPage(<PendingEventsPage />, PATH);
      await user.click(await screen.findByRole("link", { name: "Moon concert" }));
      const review = screen.getByRole("dialog", { name: "Moon concert" });
      await user.click(within(review).getByRole("button", { name: "Approve" }));
      expect((await within(review).findByRole("alert")).textContent).toBe("The hangout changed. Refresh.");
      expect(screen.queryByText("Event/Experience approved successfully")).toBeNull();
      expect(readsOf(EVENTS)).toBe(1);
      expect(within(screen.getByRole("region", { name: "Pending Events" })).getByRole("link", { name: "Moon concert" })).toBeTruthy();
      await user.click(within(review).getByRole("button", { name: "Approve" }));
      expect(await screen.findByText("Event/Experience approved successfully")).toBeTruthy();
    });
  });

  it("should require a reason before rejecting", async () => {
    renderAdminPage(<PendingEventsPage />, PATH);
    await user.click(await screen.findByRole("link", { name: "Moon concert" }));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Reject" }));
    const reject = screen.getByRole("dialog", { name: "Reject Reason*" });
    await user.click(within(reject).getByRole("button", { name: "Submit" }));
    expect(within(reject).getByRole("alert").textContent).toBe("Select at least one reason");
    expect(postJSON).not.toHaveBeenCalled();
  });

  it("should reject with the chosen reasons and the note", async () => {
    renderAdminPage(<PendingEventsPage />, PATH);
    await user.click(await screen.findByRole("link", { name: "Moon concert" }));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Reject" }));
    const reject = screen.getByRole("dialog", { name: "Reject Reason*" });
    await user.click(within(reject).getByRole("checkbox", { name: /Invalid image/ }));
    await user.click(within(reject).getByRole("checkbox", { name: /Invalid location/ }));
    await user.type(within(reject).getByRole("textbox", { name: "Other (optional)" }), "Wrong parcel");
    await user.click(within(reject).getByRole("button", { name: "Submit" }));
    expect(await screen.findByText("Event/Experience rejected successfully")).toBeTruthy();
    expect(postJSON).toHaveBeenCalledWith(...moderation("pending-one", { rejected: true, rejection_reason: "Invalid image, Invalid location. Wrong parcel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("should return to the review when the rejection is cancelled", async () => {
    renderAdminPage(<PendingEventsPage />, PATH);
    await user.click(await screen.findByRole("link", { name: "Moon concert" }));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Reject" }));
    await user.click(within(screen.getByRole("dialog", { name: "Reject Reason*" })).getByRole("button", { name: "Cancel" }));
    expect(screen.getByRole("dialog", { name: "Moon concert" })).toBeTruthy();
    expect(postJSON).not.toHaveBeenCalled();
  });

  describe("and the rejection fails", () => {
    beforeEach(() => { api.failNext.moderate = new Error("Rejection not applied"); });

    it("should keep the reason dialog open with the failure", async () => {
      renderAdminPage(<PendingEventsPage />, PATH);
      await user.click(await screen.findByRole("link", { name: "Moon concert" }));
      await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Reject" }));
      const reject = screen.getByRole("dialog", { name: "Reject Reason*" });
      await user.click(within(reject).getByRole("checkbox", { name: /Invalid image/ }));
      await user.click(within(reject).getByRole("button", { name: "Submit" }));
      expect((await within(reject).findByRole("alert")).textContent).toBe("Rejection not applied");
      expect(within(reject).getByRole<HTMLInputElement>("checkbox", { name: /Invalid image/ }).checked).toBe(true);
      expect(screen.queryByText("Event/Experience rejected successfully")).toBeNull();
    });
  });

  describe("and the signed-in wallet changed in another tab", () => {
    it("should ask to sign in instead of signing with the other wallet", async () => {
      renderAdminPage(<PendingEventsPage />, PATH);
      await user.click(await screen.findByRole("link", { name: "Moon concert" }));
      auth.identity = makeIdentity("0x3333333333333333333333333333333333333333");
      await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Approve" }));
      expect(openSignIn).toHaveBeenCalled();
      expect(postJSON).not.toHaveBeenCalled();
    });
  });
});
