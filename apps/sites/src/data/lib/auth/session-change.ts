import { useEffect, useRef } from "react";
import { useRevalidator, useRouteLoaderData } from "react-router";

/**
 * The verified-wallet cookie (`dcl_wallet_verified`) changes only through `POST /auth/callback`, a
 * plain fetch the router never sees, so root's data (the session's verified wallet, which page views
 * send as `userId`) would stay as it was. A completed post marks the session changed; root's
 * shouldRevalidate reloads its data while a change has not reached it.
 *
 * The mark is cleared when root's data reloads after a shouldRevalidate that asked for it, not inside
 * shouldRevalidate: React Router also calls it to decide what a `prefetch="intent"` link preloads
 * (react-router `dom/ssr/links.js`, getNewMatchesForLinks), so hovering a link would otherwise use
 * the mark up without any navigation.
 */

export const AUTH_CALLBACK_PATH = "/auth/callback";

let generation = 0;
/** The change root's data has not taken yet; 0 for none. */
let pending = 0;
/** The change a root reload was last asked for. */
let requested = 0;
let revalidateRoot: (() => void) | null = null;

/** Marks the session changed, and reloads the page's data now when `revalidate` is set. */
export function markSessionChanged(opts: { revalidate?: boolean } = {}): void {
  generation += 1;
  pending = generation;
  if (opts.revalidate) revalidateRoot?.();
}

/** For root's shouldRevalidate: true while a change has not reached root's data. */
export function sessionChangePending(): boolean {
  if (!pending) return false;
  requested = pending;
  return true;
}

/** Root's data reloaded: the change a reload was asked for has reached it. */
export function sessionChangeApplied(): void {
  if (pending && requested === pending) pending = 0;
}

/**
 * Posts to `/auth/callback`, which sets or clears the verified-wallet cookie on every answer it
 * gives; a post that got an answer marks the session changed, one that failed on the way (offline,
 * aborted) does not, since no cookie changed. Resolves to the answer, or null.
 */
export async function postAuthCallback(
  body: { token: string } | { signout: true },
  opts: { keepalive?: boolean; revalidate?: boolean } = {},
): Promise<Response | null> {
  let response: Response;
  try {
    response = await fetch(AUTH_CALLBACK_PATH, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      ...(opts.keepalive ? { keepalive: true } : {}),
    });
  } catch {
    return null;
  }
  markSessionChanged({ revalidate: opts.revalidate });
  return response;
}

/**
 * Mounted once, inside the router: lets a session change reload the page's data in place, and marks
 * the change applied when root's data reloads.
 */
export function useSessionChangeRevalidation(): void {
  const revalidator = useRevalidator();
  const latest = useRef(revalidator);
  latest.current = revalidator;
  useEffect(() => {
    const revalidate = () => void latest.current.revalidate();
    revalidateRoot = revalidate;
    return () => {
      if (revalidateRoot === revalidate) revalidateRoot = null;
    };
  }, []);
  const rootData = useRouteLoaderData("root");
  const seen = useRef(rootData);
  useEffect(() => {
    if (seen.current === rootData) return;
    seen.current = rootData;
    sessionChangeApplied();
  }, [rootData]);
}
