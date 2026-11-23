import { data } from "react-router";

import { CatalystError } from "@/data/lib/catalyst/client";
import { fetchPhoto, fetchUserPhotos, isPhotoId } from "@/data/lib/catalyst/social/photos";
import { isEthAddress, normalizeAddress } from "@/data/lib/catalyst/social/profile";

export const reelHeaders = { "Cache-Control": "no-store" };

function missingPhoto(): Response {
  return new Response(null, { status: 404, headers: reelHeaders });
}

function unavailable(error: unknown, signal: AbortSignal): never {
  if (signal.aborted) throw error;
  throw new Response(null, { status: 503, headers: reelHeaders });
}

export async function loadReel(id: string, signal: AbortSignal, read = fetchPhoto) {
  if (!isPhotoId(id)) throw missingPhoto();
  const photo = await read(id.toLowerCase(), { signal }).catch((error: unknown) => unavailable(error, signal));
  if (!photo || photo.id.toLowerCase() !== id.toLowerCase()) throw missingPhoto();
  return data(photo, {
    headers: photo.isPublic ? reelHeaders : { ...reelHeaders, "X-Robots-Tag": "noindex" },
  });
}

export async function loadReelGallery(address: string, signal: AbortSignal, read = fetchUserPhotos) {
  const owner = normalizeAddress(address);
  if (!isEthAddress(owner)) throw missingPhoto();
  const page = await read(owner, { limit: 24 }, { signal }).catch((error: unknown) => {
    if (!signal.aborted && error instanceof CatalystError && error.status === 404) return null;
    return unavailable(error, signal);
  });
  if (!page) return [];
  const photos = page.items.filter((photo) => photo.isPublic && isPhotoId(photo.id));
  if (page.received > 0 && photos.length === 0) unavailable(new Error("Invalid public gallery"), signal);
  return photos;
}
