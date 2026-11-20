import { describe, expect, it } from "vitest";

import { loader } from "./events_.$id.rsvp";

function target(id: string | undefined): { status: number; location: string | null } {
  const response = loader({ params: { id } });
  return { status: response.status, location: response.headers.get("Location") };
}

describe("when an old RSVP link opens", () => {
  it("should open that event's detail on the Events page", () => {
    expect(target("e5923218-67fe-43b6-897e-c431df11ac54")).toEqual({
      status: 302,
      location: "/events?id=e5923218-67fe-43b6-897e-c431df11ac54",
    });
  });

  it("should encode an id that is not URL-safe", () => {
    expect(target("a b&c").location).toBe("/events?id=a%20b%26c");
  });

  it("should open the Events page when the id is blank", () => {
    expect(target("  ").location).toBe("/events");
    expect(target(undefined).location).toBe("/events");
  });
});
