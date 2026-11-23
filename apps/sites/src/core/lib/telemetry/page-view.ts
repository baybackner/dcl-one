import { useEffect, useRef } from "react";
import { useLocation, useRouteLoaderData } from "react-router";

import { page } from "./track";

/** A Segment page call's name and properties, as the legacy site's `usePageViewTracking` took them. */
export type PageView = { name: string; properties?: Record<string, unknown> };

/**
 * The paths whose page view the page itself owns (blog, cast, social, places, storage), or that only
 * redirect (`/whats-on`, `/discover`, `/events/event`): the root's `page(pathname)` skips them. Ported
 * verbatim from the legacy site's `isPageTrackingExempt`, so `/events` itself is tracked.
 */
export function isPageTrackingExempt(pathname: string): boolean {
  if (pathname === "/blog" || pathname.startsWith("/blog/")) return true;
  if (pathname === "/whats-on" || pathname.startsWith("/whats-on/")) return true;
  if (pathname === "/discover" || pathname.startsWith("/discover/")) return true;
  if (pathname === "/events/event") return true;
  if (pathname === "/cast" || pathname.startsWith("/cast/")) return true;
  if (pathname === "/storage" || pathname.startsWith("/storage/")) return true;
  if (pathname === "/social" || pathname.startsWith("/social/")) return true;
  if (pathname === "/places" || pathname.startsWith("/places/")) return true;
  return false;
}

/**
 * Surfaces the legacy site never served, so its Segment source never counted them: the Shop and
 * Marketplace (the Shop reports its own views to its own source), the Builder, the Explorer and
 * auth or internal endpoints. Their pages send no Sites page view.
 */
const OUTSIDE_LEGACY_SITES = [
  "/shop",
  "/shop-native",
  "/marketplace",
  "/marketplace-native",
  "/builder",
  "/play",
  "/explorer",
  "/explorer-map",
  "/client",
  "/creator-hub",
  "/editor-scene",
  "/auth",
  "/connect",
  "/internal",
  "/api",
];

export function isOutsideLegacySites(pathname: string): boolean {
  return OUTSIDE_LEGACY_SITES.some((prefix) => pathname === prefix || pathname.startsWith(`${prefix}/`));
}

type RootSession ={ sid?: string | null; wallet?: string | null };

/**
 * Sends `page(name, properties)` once the view is known (a null view waits, as a page whose name is
 * its data does until the data is there) and again only when the pathname, the name or the
 * properties change: a search-only navigation, a re-render or StrictMode's second effect sends
 * nothing. The session id and verified wallet come from the root loader; without a session id
 * nothing is sent.
 */
export function usePageView(view: PageView | null | undefined): void {
  const { pathname } = useLocation();
  const root = useRouteLoaderData("root") as RootSession | undefined;
  const sid = root?.sid ?? "";
  const wallet = root?.wallet ?? "";
  const key = view?.name ? JSON.stringify([pathname, view.name, view.properties ?? {}]) : null;
  const sent = useRef<string | null>(null);
  useEffect(() => {
    if (!key || !sid || sent.current === key) return;
    sent.current = key;
    const [, name, properties] = JSON.parse(key) as [string, string, Record<string, unknown>];
    page(name, properties, { sid, ...(wallet ? { userId: wallet } : {}) });
  }, [key, sid, wallet]);
}

/** The root's page view: `page(pathname)`, except under `isPageTrackingExempt` and `isOutsideLegacySites`. */
export function useRoutePageView(): void {
  const { pathname } = useLocation();
  usePageView(isPageTrackingExempt(pathname) || isOutsideLegacySites(pathname) ? null : { name: pathname });
}

/** The 404 page's own view, `page('Not Found', { path })`, outside the paths whose pages own theirs. */
export function useNotFoundPageView(notFound: boolean): void {
  const { pathname } = useLocation();
  usePageView(notFound && !isPageTrackingExempt(pathname) ? { name: "Not Found", properties: { path: pathname } } : null);
}
