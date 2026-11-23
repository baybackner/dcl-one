// @vitest-environment jsdom
import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { emptyDraft, type HangoutDraft } from "@/data/lib/catalyst/landings/submit-hangout";
import { openSignIn } from "../../components/auth/signin-store";
import { saveHangoutSignInDraft, takeHangoutSignInDraft, useHangoutSignInDraft } from "./sign-in-draft";

const EDITOR = "/events/new-event";
const KEY = "dcl:hangout:sign-in-draft:v1";

describe("when the hangout editor keeps its draft across a browser sign-in", () => {
  let draft: HangoutDraft;

  beforeEach(() => {
    draft = { ...emptyDraft(), name: "Moon party", contact: "host@example.test", imagePreviewUrl: "https://example.test/cover.png" };
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    sessionStorage.clear();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  describe("and the visitor comes back to the same editor", () => {
    beforeEach(() => { saveHangoutSignInDraft(EDITOR, draft); });

    it("should give the draft back once", () => {
      expect(takeHangoutSignInDraft(EDITOR)).toEqual(draft);
      expect(takeHangoutSignInDraft(EDITOR)).toBeNull();
    });
  });

  describe("and the visitor lands on another editor", () => {
    beforeEach(() => { saveHangoutSignInDraft(EDITOR, draft); });

    it("should give nothing and keep the draft for its own editor", () => {
      expect(takeHangoutSignInDraft("/events/edit-event/other")).toBeNull();
      expect(takeHangoutSignInDraft(EDITOR)).toEqual(draft);
    });
  });

  describe("and the draft is older than the sign-in window", () => {
    beforeEach(() => {
      vi.useFakeTimers({ toFake: ["Date"] });
      saveHangoutSignInDraft(EDITOR, draft);
      vi.setSystemTime(Date.now() + 31 * 60 * 1000);
    });

    it("should drop it", () => {
      expect(takeHangoutSignInDraft(EDITOR)).toBeNull();
      expect(sessionStorage.getItem(KEY)).toBeNull();
    });
  });

  describe.each([
    ["is not JSON", "{"],
    ["does not hold a draft", JSON.stringify({ draft: { name: 1 }, returnTo: EDITOR, expiresAt: Number.MAX_SAFE_INTEGER })],
  ])("and the stored value %s", (_, value) => {
    beforeEach(() => { sessionStorage.setItem(KEY, value); });

    it("should give nothing", () => {
      expect(takeHangoutSignInDraft(EDITOR)).toBeNull();
    });
  });

  describe("and the browser refuses storage", () => {
    beforeEach(() => {
      vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new DOMException("Quota exceeded", "QuotaExceededError"); });
      vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new DOMException("Denied", "SecurityError"); });
    });

    it("should report the draft as not saved and read nothing, without throwing", () => {
      expect(saveHangoutSignInDraft(EDITOR, draft)).toBe(false);
      expect(takeHangoutSignInDraft(EDITOR)).toBeNull();
    });
  });

  describe("and the editor's draft is followed while sign-in leaves the app", () => {
    beforeEach(() => {
      vi.stubGlobal("location", { origin: "https://example.org", pathname: EDITOR, search: "", hash: "", assign: vi.fn() });
      const { result } = renderHook(() => useHangoutSignInDraft(EDITOR));
      result.current.remember(draft);
    });

    it("should save it when sign-in returns to this editor", () => {
      openSignIn({ redirectTo: EDITOR });
      expect(takeHangoutSignInDraft(EDITOR)).toEqual(draft);
    });

    it("should save nothing when sign-in returns elsewhere", () => {
      openSignIn({ redirectTo: "/events" });
      expect(sessionStorage.getItem(KEY)).toBeNull();
    });
  });
});
