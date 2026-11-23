import { REELS_TITLE, ReelScreen } from "@/features/reels/ReelScreen";
import { loadReel, reelHeaders } from "@/features/reels/loader.server";
import { photoLabel, reelHref } from "@/features/reels/photo-view";

import { absoluteImage, absoluteUrl, publicBase, socialMeta } from "../lib/social-meta";
import type { Route } from "./+types/reels.$imageId";

export { ReelErrorBoundary as ErrorBoundary } from "@/features/reels/ReelScreen";

export function loader({ params, request }: Route.LoaderArgs) {
  return loadReel(params.imageId, request.signal);
}

export function headers({ loaderHeaders }: Route.HeadersArgs) {
  const headers = new Headers(loaderHeaders);
  headers.set("Cache-Control", reelHeaders["Cache-Control"]);
  return headers;
}

export function meta({ loaderData: photo, matches }: Route.MetaArgs) {
  if (!photo?.isPublic) return [{ title: REELS_TITLE }, { name: "robots", content: "noindex" }];
  const base = publicBase(matches);
  return socialMeta({
    title: `${photoLabel(photo)} | ${REELS_TITLE}`,
    image: absoluteImage(base, photo.thumbnailUrl),
    url: absoluteUrl(base, reelHref(photo.id)),
  });
}

export default function ReelsDetail({ loaderData, matches }: Route.ComponentProps) {
  return <ReelScreen key={loaderData.id} photo={loaderData} origin={publicBase(matches)} />;
}
