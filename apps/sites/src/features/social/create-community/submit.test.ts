import { afterEach, describe, expect, it, vi } from "vitest";

const state = vi.hoisted(() => ({ openSignIn: vi.fn() }));
vi.mock("../../components/auth/signin-store", () => ({ openSignIn: state.openSignIn }));

import { emptyDraft } from "@/data/lib/catalyst/social/create-community";

import { submitWith } from "./submit";

const input = { name: "Builders", description: "We build", privacy: "private" as const, visibility: "unlisted" as const };

afterEach(() => {
  vi.resetAllMocks();
});

describe("submitWith", () => {
  describe("when the visitor is signed out", () => {
    it("should open the sign-in and reject without sending", async () => {
      await expect(submitWith(undefined)(input)).rejects.toThrow("Sign in to create a community.");
      expect(state.openSignIn).toHaveBeenCalledTimes(1);
    });
  });

  describe("when the visitor is signed in", () => {
    it("should send the form's fields as a draft through the route's create", async () => {
      const create = vi.fn().mockResolvedValue({ id: "c1", name: "Builders", privacy: "private", visibility: "unlisted" });
      await expect(submitWith(create)(input)).resolves.toMatchObject({ id: "c1" });
      expect(create).toHaveBeenCalledWith({ draft: { ...emptyDraft(), ...input } });
      expect(state.openSignIn).not.toHaveBeenCalled();
    });

    it("should pass on the create's failure", async () => {
      const failure = Object.assign(new Error("nope"), { status: 500 });
      await expect(submitWith(vi.fn().mockRejectedValue(failure))(input)).rejects.toBe(failure);
    });
  });
});
