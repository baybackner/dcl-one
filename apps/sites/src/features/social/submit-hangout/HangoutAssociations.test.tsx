import { renderToString } from "react-dom/server";
import { beforeEach, describe, expect, it } from "vitest";
import { emptyDraft, type HangoutDraft } from "@/data/lib/catalyst/landings/submit-hangout";
import type { HangoutOptions } from "@/data/lib/catalyst/landings/hangout-options";
import HangoutAssociations from "./HangoutAssociations";

describe("when the actual association controls render", () => {
  let draft: HangoutDraft;
  let options: HangoutOptions;
  let html: string;
  beforeEach(() => {
    draft = { ...emptyDraft(), communityId: "mine", scheduleIds: ["removed"] };
    options = { profile: null, profileError: null, communities: [{ id: "mine", name: "My managed community" }], communitiesError: null, schedules: [], schedulesError: null, worlds: [], worldsError: null };
  });

  it("should offer loaded communities and real asset search without privileged controls", () => {
    html = renderToString(<HangoutAssociations draft={draft} options={options} canEditAny={false} canChangeCommunity onEdit={() => {}} />);
    expect(html).toContain("My managed community");
    expect(html).toContain("Search featured items or collections");
    expect(html).not.toContain("Custom join link");
    expect(html).not.toContain("Unavailable schedule");
  });

  describe("and the wallet may edit any event", () => {
    it("should show campaign removal and custom link controls while locking owner community changes", () => {
      html = renderToString(<HangoutAssociations draft={draft} options={options} canEditAny canChangeCommunity={false} onEdit={() => {}} />);
      expect(html).toContain("Unavailable schedule (remove association)");
      expect(html).toContain("Custom join link");
      expect(html).toMatch(/<select[^>]*disabled/);
    });
  });

  describe("and the communities and schedules reads failed", () => {
    beforeEach(() => {
      options = { ...options, communitiesError: "Social down", schedulesError: "Schedules down" };
      html = renderToString(<HangoutAssociations draft={draft} options={options} canEditAny canChangeCommunity onEdit={() => {}} />);
    });

    it("should report each failure in an error notice", () => {
      expect(html).toMatch(/<p[^>]*data-tone="error"[^>]*role="alert"[^>]*>Could not load your communities: Social down<\/p>/);
      expect(html).toMatch(/<p[^>]*data-tone="error"[^>]*role="alert"[^>]*>Could not load schedules: Schedules down<\/p>/);
    });
  });

  describe("and the options are still loading", () => {
    it("should keep a plain loading line, not a notice", () => {
      html = renderToString(<HangoutAssociations draft={draft} options={null} canEditAny canChangeCommunity onEdit={() => {}} />);
      expect(html).toContain('<p role="status">Loading communities and schedules…</p>');
      expect(html).not.toContain("data-tone");
    });
  });
});
