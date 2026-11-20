import { redirect } from "react-router";

import { EVENT_ID_PARAM, EVENTS_PATH } from "@/features/events/view";

/** Going to an event is the detail's "Remind me", as on decentraland.org: an old RSVP link opens that detail. */
export function loader({ params }: { params: { id?: string } }) {
  const id = params.id?.trim();
  return redirect(id ? `${EVENTS_PATH}?${EVENT_ID_PARAM}=${encodeURIComponent(id)}` : EVENTS_PATH);
}
