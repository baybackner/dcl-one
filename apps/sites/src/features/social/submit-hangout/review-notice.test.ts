import { describe, expect, it } from "vitest";

import { readRejectionReason, rejectionReason } from "@dcl/one-ui/apps/sites/events/admin-copy";

import { rejectedNotice } from "./review-notice";

const LEAD_TWO =
  "This event has been rejected because of Invalid image and Invalid event name. Please update the information and submit again";

describe("when an event's rejection is shown to its owner", () => {
  it("should list the reasons with 'and' and put the moderator's note on its own line", () => {
    expect(rejectedNotice("Invalid image, Invalid event name. test", "en")).toBe(`${LEAD_TWO}\ntest`);
  });

  it("should read back exactly what moderation stores", () => {
    const stored = rejectionReason(["invalid_image", "invalid_duration", "invalid_location"], "  Fix the time  ");
    expect(rejectedNotice(stored, "en")).toBe(
      "This event has been rejected because of Invalid image, Invalid duration, and Invalid location. Please update the information and submit again\nFix the time",
    );
  });

  it("should omit the note line when the moderator wrote none", () => {
    expect(rejectedNotice("Invalid image, Invalid event name", "en")).toBe(LEAD_TWO);
  });

  it("should keep a reason written elsewhere whole", () => {
    expect(rejectedNotice("Spam. Repeated listings", "en")).toBe(
      "This event has been rejected because of Spam. Repeated listings. Please update the information and submit again",
    );
  });

  it("should say it was rejected without a reason when none was stored", () => {
    const plain = "This event has been rejected. Please update the information and submit again";
    expect(rejectedNotice(null, "en")).toBe(plain);
    expect(rejectedNotice("   ", "en")).toBe(plain);
  });

  it("should join the reasons the way the reader's language does", () => {
    expect(rejectedNotice("Invalid image, Invalid event name", "es")).toContain("Invalid image e Invalid event name");
  });
});

describe("readRejectionReason", () => {
  it("should treat a known title followed by unknown text as one reason, not a list", () => {
    expect(readRejectionReason("Invalid image, Something else")).toEqual({ reasons: ["Invalid image, Something else"], note: "" });
  });

  it("should keep a note that has its own sentences", () => {
    expect(readRejectionReason("Invalid image. First. Second.")).toEqual({ reasons: ["Invalid image"], note: "First. Second." });
  });
});
