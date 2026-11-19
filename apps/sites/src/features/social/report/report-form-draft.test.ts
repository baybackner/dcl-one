// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";

import { saveReportFormDraft, takeReportFormDraft } from "./report-form-draft";

const draft = {
  searchParams: "player_address=0x123",
  reportedAddress: "0x456",
  reason: "",
  description: "Description",
  additionalComments: "",
} as const;

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  sessionStorage.clear();
});

describe("when restoring a report draft", () => {
  it("should discard a draft past the 15-minute lifetime", () => {
    const now = vi.spyOn(Date, "now").mockReturnValue(0);
    saveReportFormDraft(draft);
    now.mockReturnValue(15 * 60_000 + 1);
    expect(takeReportFormDraft(draft.searchParams)).toBeNull();
    expect(sessionStorage.length).toBe(0);
  });

  it("should discard a draft from a different report URL", () => {
    saveReportFormDraft(draft);
    expect(takeReportFormDraft("player_address=0x789")).toBeNull();
    expect(takeReportFormDraft(draft.searchParams)).toBeNull();
  });

  it("should ignore an invalid stored reason", () => {
    sessionStorage.setItem("report_form_draft", JSON.stringify({ ...draft, reason: "unknown", savedAt: Date.now() }));
    expect(takeReportFormDraft(draft.searchParams)).toBeNull();
  });

  it("should report that a draft could not be saved when storage is unavailable", () => {
    vi.stubGlobal("sessionStorage", undefined);
    expect(saveReportFormDraft(draft)).toBe(false);
    expect(takeReportFormDraft(draft.searchParams)).toBeNull();
  });
});
