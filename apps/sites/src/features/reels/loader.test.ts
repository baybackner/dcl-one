import { describe, expect, it, vi } from "vitest";

import { CatalystError } from "@/data/lib/catalyst/client";
import type { ProfilePhoto } from "@/data/lib/catalyst/social/photos";

import { loadReel, loadReelGallery } from "./loader.server";

const ID = "00000000-0000-0000-0000-000000000001";
const ADDRESS = "0x1111111111111111111111111111111111111111";
const signal = new AbortController().signal;
const photo = { id: ID, isPublic: false } as ProfilePhoto;

describe("Reels loader", () => {
  it("does not request malformed identifiers", async () => {
    const read = vi.fn();
    await expect(loadReel("invalid", signal, read)).rejects.toMatchObject({ status: 404 });
    await expect(loadReelGallery("invalid", signal, read)).rejects.toMatchObject({ status: 404 });
    expect(read).not.toHaveBeenCalled();
  });

  it("opens unlisted photos by exact link without identity and disables document caching", async () => {
    const read = vi.fn().mockResolvedValue(photo);
    const response = await loadReel(ID, signal, read);
    expect(read).toHaveBeenCalledWith(ID, { signal });
    expect(response.data).toEqual(photo);
    expect(new Headers(response.init?.headers).get("Cache-Control")).toBe("no-store");
    expect(new Headers(response.init?.headers).get("X-Robots-Tag")).toBe("noindex");
  });

  it("keeps missing and unavailable photos distinct", async () => {
    await expect(loadReel(ID, signal, vi.fn().mockResolvedValue(null))).rejects.toMatchObject({ status: 404 });
    await expect(
      loadReel(ID, signal, vi.fn().mockRejectedValue(new CatalystError("unavailable", "/photo", 502))),
    ).rejects.toMatchObject({ status: 503 });
  });

  it("loads only the first public page and omits unlisted or malformed IDs", async () => {
    const publicPhoto = { ...photo, isPublic: true };
    const read = vi.fn().mockResolvedValue({
      items: [photo, publicPhoto, { ...publicPhoto, id: "bad" }],
      received: 3,
      total: 3,
    });
    expect(await loadReelGallery(ADDRESS, signal, read)).toEqual([publicPhoto]);
    expect(read).toHaveBeenCalledWith(ADDRESS, { limit: 24 }, { signal });
  });

  it("distinguishes an empty gallery from invalid upstream rows", async () => {
    const read = vi
      .fn()
      .mockResolvedValueOnce({ items: [], received: 0, total: 0 })
      .mockResolvedValueOnce({ items: [], received: 1, total: 1 })
      .mockRejectedValueOnce(new CatalystError("unknown wallet", "/user", 404));
    expect(await loadReelGallery(ADDRESS, signal, read)).toEqual([]);
    await expect(loadReelGallery(ADDRESS, signal, read)).rejects.toMatchObject({ status: 503 });
    expect(await loadReelGallery(ADDRESS, signal, read)).toEqual([]);
  });

  it("preserves an aborted request", async () => {
    const controller = new AbortController();
    controller.abort();
    const failure = controller.signal.reason;
    const read = vi.fn().mockRejectedValue(failure);
    await expect(loadReel(ID, controller.signal, read)).rejects.toBe(failure);
    await expect(loadReelGallery(ADDRESS, controller.signal, read)).rejects.toBe(failure);
  });
});
