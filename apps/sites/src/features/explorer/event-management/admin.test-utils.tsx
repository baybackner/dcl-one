import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render } from "@testing-library/react";
import { Fragment, StrictMode, type ReactElement } from "react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router";
import { vi } from "vitest";

import type { AuthIdentity } from "@/data/lib/auth/types";
import { CatalystError, postJSON, signedGetJSON } from "@/data/lib/catalyst/client";
import type { EventProfile, ManagedEvent } from "@/data/lib/catalyst/places/event-management";

export const MODERATOR = "0x1111111111111111111111111111111111111111";

export function makeIdentity(signer = MODERATOR): AuthIdentity {
  return { signer, ephemeral: { address: "0x2222222222222222222222222222222222222222", privateKey: "0x01" }, expiration: "2099-01-01T00:00:00Z", authChain: [] };
}

/** A complete Events API record, so the real loaders parse it. */
export function makeEvent(overrides: Partial<ManagedEvent> = {}): ManagedEvent {
  return {
    id: "pending-one", name: "Moon concert", image: "https://example.com/cover.png", image_vertical: null, description: "A concert",
    start_at: "2099-01-01T18:00:00Z", finish_at: "2099-01-01T20:00:00Z", next_start_at: null, next_finish_at: null, duration: null,
    all_day: false, x: 0, y: 0, server: "moon.dcl.eth", url: null, user: MODERATOR, user_name: "Luna",
    estate_id: null, estate_name: null, scene_name: null, approved: false, rejected: false,
    highlighted: false, trending: false, world: true, recurrent: false, recurrent_frequency: null,
    recurrent_weekday_mask: 0, recurrent_month_mask: 0, recurrent_interval: 1, recurrent_setpos: null,
    recurrent_monthday: null, recurrent_count: null, recurrent_until: null, recurrent_dates: [],
    categories: [], schedules: [], total_attendees: 0, latest_attendees: [], coordinates: [0, 0],
    position: [0, 0], live: false, attending: false, place_id: null, community_id: null,
    featured_item: null, created_at: null, updated_at: new Date().toISOString(), approved_by: null, rejected_by: null,
    rejection_reason: null, deleted_by_user: false, deleted_by_admin: false, deleted_by: null,
    deleted_at: null, deleted_reason: null, previous_place_id: null,
    ...overrides,
  };
}

export type FakeEventsApi = {
  profile: EventProfile;
  /** The moderator-only list; `"forbidden"` answers 403 as for an account that is not a moderator. */
  profiles: EventProfile[] | "forbidden";
  events: ManagedEvent[];
  /** Fails the next request of each kind once, with this error. */
  failNext: Partial<Record<"profile" | "events" | "moderate" | "save", Error>>;
};

/**
 * Answers the Events API at the Catalyst client boundary (the test mocks `signedGetJSON` and `postJSON`
 * of `@/data/lib/catalyst/client` with `vi.fn()`), so the real loaders, parsers and queries run.
 */
export function fakeEventsApi(api: FakeEventsApi) {
  const take = (kind: keyof FakeEventsApi["failNext"]) => {
    const error = api.failNext[kind];
    delete api.failNext[kind];
    if (error) throw error;
  };
  vi.mocked(signedGetJSON).mockImplementation(async (path: string) => {
    if (path === "/events/api/profiles/me/settings") {
      take("profile");
      return { ok: true, data: api.profile } as never;
    }
    if (path === "/events/api/profiles/settings") {
      if (api.profiles === "forbidden") throw new CatalystError("Forbidden", path, 403);
      return { ok: true, data: api.profiles } as never;
    }
    if (path === "/events/api/events") {
      take("events");
      return { ok: true, data: api.events } as never;
    }
    throw new Error(`Unexpected read ${path}`);
  });
  vi.mocked(postJSON).mockImplementation(async (path: string, body: unknown) => {
    if (/^\/events\/api\/events\/[^/]+$/.test(path)) {
      take("moderate");
      const patch = body as { approved?: boolean; rejected?: boolean };
      return { ok: true, data: { approved: Boolean(patch.approved), rejected: Boolean(patch.rejected) } } as never;
    }
    const profile = /^\/events\/api\/profiles\/([^/]+)\/settings$/.exec(path);
    if (profile) {
      take("save");
      return { ok: true, data: { user: profile[1], permissions: (body as { permissions: string[] }).permissions } } as never;
    }
    throw new Error(`Unexpected write ${path}`);
  });
}

/** How many reads of one Events API path were made. */
export const readsOf = (path: string) => vi.mocked(signedGetJSON).mock.calls.filter(([called]) => called === path).length;

function Location() {
  const location = useLocation();
  return <output data-testid="location">{location.pathname + location.search}</output>;
}

/**
 * Renders a page at `path` beside a /events probe, under a fresh query client. `strict` wraps it in
 * StrictMode, as the Sites client entry renders it, so effects run, clean up and run again as in
 * development; it also repeats the page's reads, so tests that count reads leave it off.
 */
export function renderAdminPage(page: ReactElement, path: string, { strict = false } = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const Wrap = strict ? StrictMode : Fragment;
  const view = render(
    <Wrap>
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <Routes>
          <Route path="/events" element={<p>Events home</p>} />
          <Route path="/events/edit-event/:id" element={<p>Edit page</p>} />
          <Route path="*" element={page} />
        </Routes>
        <Location />
      </MemoryRouter>
    </QueryClientProvider>
    </Wrap>,
  );
  return { client, view };
}
