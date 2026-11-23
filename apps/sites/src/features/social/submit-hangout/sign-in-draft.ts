import { useCallback, useEffect, useRef, useState } from "react";
import { z } from "zod";

import type { HangoutDraft } from "@/data/lib/catalyst/landings/submit-hangout";
import { onBeforeBrowserSignIn } from "../../components/auth/signin-store";

const KEY = "dcl:hangout:sign-in-draft:v1";
const MAX_AGE_MS = 30 * 60 * 1000;

const Draft = z.object({
  name: z.string(),
  description: z.string(),
  startDate: z.string(),
  startTime: z.string(),
  durationHours: z.number(),
  allDay: z.boolean(),
  location: z.enum(["land", "world"]),
  coordX: z.number(),
  coordY: z.number(),
  worldName: z.string(),
  recurrent: z.boolean(),
  recurrence: z.string(),
  recurrentUntil: z.string(),
  recurrentCount: z.number().nullable().optional(),
  category: z.string(),
  communityId: z.string(),
  contact: z.string(),
  details: z.string(),
  imagePreviewUrl: z.string().nullable(),
  verticalImagePreviewUrl: z.string().nullable(),
  featuredItem: z.string().optional(),
}) satisfies z.ZodType<HangoutDraft>;

const Stored = z.object({ draft: Draft, returnTo: z.string(), expiresAt: z.number() });

/** Keeps the editor's draft for this tab across the sign-in page, which leaves the app. */
export function saveHangoutSignInDraft(returnTo: string, draft: HangoutDraft): boolean {
  try {
    sessionStorage.setItem(KEY, JSON.stringify({ draft, returnTo, expiresAt: Date.now() + MAX_AGE_MS }));
    return true;
  } catch {
    return false;
  }
}

/** The draft saved for this same editor URL, removed once read; an expired or unreadable one is dropped. */
export function takeHangoutSignInDraft(returnTo: string): HangoutDraft | null {
  try {
    const value = sessionStorage.getItem(KEY);
    if (value === null) return null;
    const result = Stored.safeParse(JSON.parse(value));
    if (!result.success || result.data.expiresAt <= Date.now()) {
      sessionStorage.removeItem(KEY);
      return null;
    }
    if (result.data.returnTo !== returnTo) return null;
    sessionStorage.removeItem(KEY);
    return result.data.draft;
  } catch {
    return null;
  }
}

/**
 * The editor's draft across a browser sign-in: `remember` follows the draft on screen, which is saved when
 * sign-in leaves for this editor's URL, and `restored` is the draft saved for it, read once back here.
 */
export function useHangoutSignInDraft(returnTo: string) {
  const [restored, setRestored] = useState<HangoutDraft | null>(null);
  const latest = useRef<HangoutDraft | null>(null);
  useEffect(() => {
    const found = takeHangoutSignInDraft(returnTo);
    if (found) setRestored(found);
  }, [returnTo]);
  useEffect(() => onBeforeBrowserSignIn((destination) => {
    if (destination === returnTo && latest.current) saveHangoutSignInDraft(returnTo, latest.current);
  }), [returnTo]);
  const remember = useCallback((draft: HangoutDraft) => { latest.current = draft; }, []);
  return { restored, remember };
}
