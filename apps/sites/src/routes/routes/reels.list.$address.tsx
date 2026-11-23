import { ReelsEmpty, ReelsGallery, ReelsPage } from "@dcl/one-ui/apps/sites/reels/ReelsPage";

import { REELS_TITLE, ReelLink } from "@/features/reels/ReelScreen";
import { loadReelGallery, reelHeaders } from "@/features/reels/loader.server";
import { photoLabel, reelHref } from "@/features/reels/photo-view";

import type { Route } from "./+types/reels.list.$address";

export { ReelErrorBoundary as ErrorBoundary } from "@/features/reels/ReelScreen";

export function loader({ params, request }: Route.LoaderArgs) {
  return loadReelGallery(params.address, request.signal);
}

export function headers() {
  return reelHeaders;
}

export function meta() {
  return [{ title: REELS_TITLE }, { name: "robots", content: "noindex" }];
}

export default function ReelsList({ loaderData, params }: Route.ComponentProps) {
  const name = loaderData[0]?.author.name || params.address;
  const photos = loaderData.map((photo) => ({
    id: photo.id,
    href: reelHref(photo.id),
    label: photoLabel(photo),
    thumbnail: photo.thumbnailUrl,
    image: photo.url,
  }));
  return (
    <ReelsPage title={`Reels by ${name}`} gallery>
      {photos.length ? <ReelsGallery photos={photos} linkComponent={ReelLink} /> : <ReelsEmpty reason="gallery" />}
    </ReelsPage>
  );
}
