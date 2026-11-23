import { useCallback, useEffect, useRef, useState } from "react";
import { z } from "zod";

import type { HangoutDraft } from "@/data/lib/catalyst/landings/submit-hangout";
import { onBeforeBrowserSignIn } from "../../components/auth/signin-store";
import { signInDraftStore } from "../../components/auth/sign-in-draft-store";

const Draft = z.strictObject({
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

// A field added to HangoutDraft and not to Draft fails here, instead of being dropped from a kept draft.
type Unkept = Exclude<keyof HangoutDraft, keyof z.infer<typeof Draft>>;
const everyFieldKept: [Unkept] extends [never] ? true : Unkept = true;
void everyFieldKept;

/** The draft and the account it was written for: only that account gets it back. */
const Kept = z.object({ account: z.string(), draft: Draft });
export type KeptHangoutDraft = z.infer<typeof Kept>;

const store = signInDraftStore("dcl:hangout:sign-in-draft:v2", Kept, 30 * 60 * 1000);

export const saveHangoutSignInDraft = (returnTo: string, kept: KeptHangoutDraft): boolean => store.save(returnTo, kept);
export const takeHangoutSignInDraft = (returnTo: string): KeptHangoutDraft | null => store.take(returnTo);

/**
 * The editor's draft across a browser sign-in. `remember` follows the draft on screen; when sign-in leaves
 * for this editor's URL, a draft that differs from `initial` is saved for `account`. Storage that refuses
 * the save stops that first sign-in and sets `saveFailed`, so the writer can copy the text; the next sign-in
 * goes ahead. `restored` is the draft kept for this URL, offered only while `account` is the one that wrote it.
 */
export function useHangoutSignInDraft(returnTo: string, account: string | null, initial: HangoutDraft) {
  const [kept, setKept] = useState<KeptHangoutDraft | null>(null);
  const [saveFailed, setSaveFailed] = useState(false);
  const latest = useRef<HangoutDraft | null>(null);
  const initialRef = useRef(initial);
  const accountRef = useRef(account);
  const warned = useRef(false);
  useEffect(() => {
    initialRef.current = initial;
    accountRef.current = account;
  });
  useEffect(() => {
    const found = takeHangoutSignInDraft(returnTo);
    if (found) setKept(found);
  }, [returnTo]);
  // Another account signing in drops the kept draft for good, so it never comes back to anyone.
  useEffect(() => {
    if (kept && account && account !== kept.account) setKept(null);
  }, [account, kept]);
  useEffect(() => onBeforeBrowserSignIn((destination) => {
    const draft = latest.current;
    const owner = accountRef.current;
    if (destination !== returnTo || !draft || !owner) return;
    if (JSON.stringify(draft) === JSON.stringify(initialRef.current)) return;
    if (saveHangoutSignInDraft(returnTo, { account: owner, draft })) return;
    if (warned.current) return;
    warned.current = true;
    setSaveFailed(true);
    return false;
  }), [returnTo]);
  const remember = useCallback((draft: HangoutDraft) => { latest.current = draft; }, []);
  const restored = kept && kept.account === account ? kept.draft : null;
  return { restored, remember, saveFailed };
}
