import type { CommunityCreateInput } from "@dcl/one-ui/apps/sites/social/CommunityCreate";

import { emptyDraft } from "@/data/lib/catalyst/social/create-community";
import { openSignIn } from "../../components/auth/signin-store";

import type { CreateFn } from "./machine";

/** The form's submit: the route's signed create, or the sign-in prompt for a signed-out visitor. */
export function submitWith(create: CreateFn | undefined) {
  return async (input: CommunityCreateInput) => {
    if (!create) {
      openSignIn();
      throw new Error("Sign in to create a community.");
    }
    return create({ draft: { ...emptyDraft(), ...input } });
  };
}
