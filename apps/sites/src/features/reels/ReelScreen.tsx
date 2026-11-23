import { useQuery } from "@tanstack/react-query";
import { isRouteErrorResponse, Link, useRevalidator, useRouteError } from "react-router";

import type { PlaceCardLinkProps } from "@dcl/one-ui";
import { PhotoViewer } from "@dcl/one-ui/apps/sites/profile/PhotoViewer";
import { ReelsEmpty, ReelsPage } from "@dcl/one-ui/apps/sites/reels/ReelsPage";

import type { ProfilePhoto } from "@/data/lib/catalyst/social/photos";
import { canReadProfile, placeOwnersQuery } from "@/data/lib/profile/queries";
import { reelPlaceQuery, reelWearables, reelWearablesQuery } from "@/data/lib/reels/queries";

import { photoDestination, photoDetails, photoLabel } from "./photo-view";
import { usePhotoActions } from "./usePhotoActions";

export const REELS_TITLE = "Decentraland Reels";

export function ReelLink(props: PlaceCardLinkProps) {
  return <Link {...props} prefetch="intent" />;
}

export function ReelScreen({ photo, origin }: { photo: ProfilePhoto; origin?: string }) {
  const { position, realm } = photoDestination(photo);
  const addresses = [photo.author.address, ...photo.people.map((person) => person.address)].filter(canReadProfile);
  const faces = useQuery({ ...placeOwnersQuery(addresses), enabled: addresses.length > 0 });
  const place = useQuery(reelPlaceQuery(position, realm));
  const worn = useQuery(reelWearablesQuery(photo.people.flatMap((person) => person.wearables ?? [])));
  const details = photoDetails(photo, faces.data ?? {}, place.data ?? undefined, (urns) =>
    urns.length && !worn.isLoading ? reelWearables(urns, worn.data ?? {}) : undefined,
  );
  const actions = usePhotoActions(photo, true, origin);
  const title = photoLabel(photo);

  return (
    <ReelsPage title={title}>
      <PhotoViewer
        imageUrl={photo.url}
        alt={title}
        label={title}
        details={details}
        actions={actions}
        linkComponent={ReelLink}
      />
    </ReelsPage>
  );
}

export function ReelErrorBoundary() {
  const error = useRouteError();
  const revalidator = useRevalidator();
  const missing = isRouteErrorResponse(error) && error.status === 404;
  return (
    <ReelsPage title={REELS_TITLE}>
      <ReelsEmpty
        reason={missing ? "photo" : "unavailable"}
        onRetry={
          missing
            ? undefined
            : () => {
                void revalidator.revalidate();
              }
        }
      />
    </ReelsPage>
  );
}
