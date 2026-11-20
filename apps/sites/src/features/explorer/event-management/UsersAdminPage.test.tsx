// @vitest-environment jsdom
import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AuthIdentity } from "@/data/lib/auth/types";
import { postJSON } from "@/data/lib/catalyst/client";
import type { EventProfile } from "@/data/lib/catalyst/places/event-management";
import { fakeEventsApi, makeIdentity, MODERATOR, readsOf, renderAdminPage, type FakeEventsApi } from "./admin.test-utils";
import UsersAdminPage from "./UsersAdminPage";

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

const PATH = "/events/admin/users";
const NEW_WALLET = "0x4444444444444444444444444444444444444444";
const LIST = "/events/api/profiles/settings";
const saving = (wallet: string, permissions: string[]) => [`/events/api/profiles/${wallet.toLowerCase()}/settings`, { permissions }, expect.objectContaining({ method: "PATCH" })];
const address = (index: number) => `0x${String(index).padStart(40, "a")}`;

describe("when opening the Users page", () => {
  let identity: AuthIdentity;
  let profile: EventProfile;
  let api: FakeEventsApi;
  let user: ReturnType<typeof userEvent.setup>;

  beforeEach(() => {
    identity = makeIdentity();
    auth.identity = identity;
    auth.isConnected = true;
    profile = { user: MODERATOR, permissions: ["approve_any_event", "edit_any_profile"] };
    api = {
      profile,
      profiles: [
        profile,
        { user: address(1), permissions: ["approve_own_event", "edit_any_event", "edit_any_schedule"] },
        ...Array.from({ length: 10 }, (_, index) => ({ user: address(index + 2), permissions: [] })),
      ],
      events: [],
      failNext: {},
    };
    fakeEventsApi(api);
    // The accounts' names come from one Catalyst profiles read.
    vi.stubGlobal("fetch", vi.fn(async () => Response.json([{ avatars: [{ ethAddress: address(1), name: "Selene" }] }])));
    user = userEvent.setup();
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    vi.resetAllMocks();
  });

  describe("and the account holds the profile permission but is not a moderator", () => {
    beforeEach(() => {
      profile.permissions = ["edit_any_profile"];
      api.profiles = "forbidden";
    });

    it("should redirect to /events", async () => {
      renderAdminPage(<UsersAdminPage />, PATH);
      await waitFor(() => expect(screen.getByTestId("location").textContent).toBe("/events"));
    });
  });

  describe("and the account has no profile permission", () => {
    beforeEach(() => { profile.permissions = ["approve_any_event"]; });

    it("should redirect to /events without reading other accounts", async () => {
      renderAdminPage(<UsersAdminPage />, PATH);
      await waitFor(() => expect(screen.getByTestId("location").textContent).toBe("/events"));
      expect(readsOf(LIST)).toBe(0);
    });
  });

  it("should list the accounts with a check per grant and the account's name", async () => {
    renderAdminPage(<UsersAdminPage />, PATH);
    const table = await screen.findByRole("table", { name: "Users" });
    expect(within(table).getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual([
      "User", "Approve Own Events", "Approve Events", "Edit Events", "Edit Users",
    ]);
    const row = (await within(table).findByRole("button", { name: `${address(1)} (Selene)` })).closest("tr")!;
    expect(within(row).getAllByRole("img", { name: "Granted" })).toHaveLength(2);
    expect(screen.getByText("1–10 of 12")).toBeTruthy();
    const bar = screen.getByRole("navigation", { name: "Event administration" });
    expect(within(bar).getByRole("link", { name: "Users" }).getAttribute("aria-current")).toBe("page");
    expect(within(bar).getByRole("link", { name: "Pending Events" })).toBeTruthy();
  });

  it("should page the accounts and filter them by wallet or name", async () => {
    renderAdminPage(<UsersAdminPage />, PATH);
    await screen.findByText("1–10 of 12");
    await user.click(screen.getByRole("button", { name: "Next page" }));
    expect(screen.getByText("11–12 of 12")).toBeTruthy();
    await user.type(screen.getByRole("searchbox", { name: "Search" }), "selene");
    expect(screen.getByText("1–1 of 1")).toBeTruthy();
    await user.clear(screen.getByRole("searchbox", { name: "Search" }));
    await user.type(screen.getByRole("searchbox", { name: "Search" }), "0xnothing");
    expect(screen.getByText("No matching users")).toBeTruthy();
    await user.selectOptions(screen.getByRole("combobox", { name: "Rows per page:" }), "25");
    expect(readsOf(LIST)).toBe(1);
  });

  describe("and no account holds a grant", () => {
    beforeEach(() => { api.profiles = []; });

    it("should say no admins are configured", async () => {
      renderAdminPage(<UsersAdminPage />, PATH);
      expect(await screen.findByText("No admins configured")).toBeTruthy();
    });
  });

  it("should keep Save Changes disabled until the wallet address is valid", async () => {
    renderAdminPage(<UsersAdminPage />, PATH);
    await user.click(await screen.findByRole("button", { name: "+ Add User" }));
    const dialog = screen.getByRole("dialog", { name: "Add User" });
    const save = within(dialog).getByRole<HTMLButtonElement>("button", { name: "Save Changes" });
    expect(save.disabled).toBe(true);
    await user.type(within(dialog).getByRole("textbox", { name: "Wallet Address" }), "0x123");
    expect(within(dialog).getByText("Enter a valid Ethereum address")).toBeTruthy();
    expect(save.disabled).toBe(true);
    await user.click(save);
    expect(postJSON).not.toHaveBeenCalled();
  });

  it("should add a user with the chosen grants and refresh the list", async () => {
    renderAdminPage(<UsersAdminPage />, PATH);
    await user.click(await screen.findByRole("button", { name: "+ Add User" }));
    const dialog = screen.getByRole("dialog", { name: "Add User" });
    await user.type(within(dialog).getByRole("textbox", { name: "Wallet Address" }), NEW_WALLET);
    await user.click(within(dialog).getByRole("switch", { name: "Approve Events" }));
    await user.click(within(dialog).getByRole("button", { name: "Save Changes" }));
    expect(await screen.findByText("Permissions updated")).toBeTruthy();
    expect(postJSON).toHaveBeenCalledWith(...saving(NEW_WALLET, ["approve_any_event"]));
    expect(screen.queryByRole("dialog")).toBeNull();
    await waitFor(() => expect(readsOf(LIST)).toBe(2));
  });

  describe("and saving fails", () => {
    beforeEach(() => { api.failNext.save = new Error("Permissions not saved"); });

    it("should keep the dialog and its choices and report the failure", async () => {
      renderAdminPage(<UsersAdminPage />, PATH);
      await user.click(await screen.findByRole("button", { name: "+ Add User" }));
      const dialog = screen.getByRole("dialog", { name: "Add User" });
      await user.type(within(dialog).getByRole("textbox", { name: "Wallet Address" }), NEW_WALLET);
      await user.click(within(dialog).getByRole("switch", { name: "Edit Events" }));
      await user.click(within(dialog).getByRole("button", { name: "Save Changes" }));
      expect((await within(dialog).findByRole("alert")).textContent).toBe("Permissions not saved");
      expect(within(dialog).getByRole<HTMLInputElement>("switch", { name: "Edit Events" }).checked).toBe(true);
      expect(screen.queryByText("Permissions updated")).toBeNull();
      expect(readsOf(LIST)).toBe(1);
    });
  });

  it("should edit a user from its row and keep the grants the dialog does not show", async () => {
    renderAdminPage(<UsersAdminPage />, PATH);
    await user.click(await screen.findByRole("button", { name: `${address(1)} (Selene)` }));
    const dialog = screen.getByRole("dialog", { name: "Edit User" });
    expect(within(dialog).getByText("Selene")).toBeTruthy();
    expect(within(dialog).queryByRole("textbox", { name: "Wallet Address" })).toBeNull();
    expect(within(dialog).getByRole<HTMLInputElement>("switch", { name: "Approve Own Events" }).checked).toBe(true);
    await user.click(within(dialog).getByRole("switch", { name: "Edit Events" }));
    await user.click(within(dialog).getByRole("button", { name: "Save Changes" }));
    await screen.findByText("Permissions updated");
    expect(postJSON).toHaveBeenCalledWith(...saving(address(1), ["approve_own_event", "edit_any_schedule"]));
  });

  it("should not let the signed-in account remove its own user administration", async () => {
    renderAdminPage(<UsersAdminPage />, PATH);
    await user.click(await screen.findByRole("button", { name: MODERATOR }));
    const dialog = screen.getByRole("dialog", { name: "Edit User" });
    expect(within(dialog).getByRole<HTMLInputElement>("switch", { name: "Edit Users" }).disabled).toBe(true);
    expect(within(dialog).getByText("You cannot remove your own admin permission")).toBeTruthy();
  });

  describe("and the server keeps a moderator's effective grants", () => {
    beforeEach(() => {
      vi.mocked(postJSON).mockResolvedValue({ ok: true, data: { user: MODERATOR, permissions: ["approve_any_event", "edit_any_profile"] } });
    });

    it("should say the server kept them instead of claiming the change", async () => {
      renderAdminPage(<UsersAdminPage />, PATH);
      await user.click(await screen.findByRole("button", { name: MODERATOR }));
      const dialog = screen.getByRole("dialog", { name: "Edit User" });
      await user.click(within(dialog).getByRole("switch", { name: "Approve Events" }));
      await user.click(within(dialog).getByRole("button", { name: "Save Changes" }));
      expect(await screen.findByText(/The server kept the account's effective permissions/)).toBeTruthy();
    });
  });

  it("should close the editor without saving on Cancel", async () => {
    renderAdminPage(<UsersAdminPage />, PATH);
    await user.click(await screen.findByRole("button", { name: "+ Add User" }));
    await user.click(within(screen.getByRole("dialog", { name: "Add User" })).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(postJSON).not.toHaveBeenCalled();
  });
});
