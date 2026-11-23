// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { track } from "@/core/lib/telemetry/track";
import { detectMacChip } from "@/data/lib/download/mac-chip";

import { DOWNLOAD_CLICK_STORAGE_KEY, resetMacArchHint, trackDownloadClick, type DownloadClick } from "./download-click";

vi.mock("@/core/lib/telemetry/track", () => ({ track: vi.fn() }));
vi.mock("@/data/lib/download/mac-chip", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/data/lib/download/mac-chip")>()),
  detectMacChip: vi.fn(() => "apple_silicon"),
}));

const WINDOWS_UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36";
const MAC_UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/120 Safari/537.36";
const CLICK: DownloadClick = { place: "Landing Hero", event: "Download", download_target: "desktop_installer" };

function setUserAgent(ua: string, maxTouchPoints = 0) {
  vi.stubGlobal("navigator", { userAgent: ua, maxTouchPoints });
  resetMacArchHint();
}

function sent() {
  expect(track).toHaveBeenCalledTimes(1);
  const [event, props, ctx] = vi.mocked(track).mock.calls[0]!;
  return { event, props: props as Record<string, unknown>, ctx };
}

beforeEach(() => {
  setUserAgent(WINDOWS_UA);
  window.history.replaceState(null, "", "/");
  sessionStorage.clear();
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  vi.mocked(track).mockReset();
  vi.mocked(detectMacChip).mockClear();
});

describe("trackDownloadClick", () => {
  it("sends the legacy Click with its attributes, audit fields and the legacy + conversion gates", () => {
    vi.spyOn(Date, "now").mockReturnValue(1_700_000_000_000);
    trackDownloadClick(CLICK, "sid-1");
    const { event, props, ctx } = sent();
    expect(event).toBe("Click");
    expect(props).toMatchObject({
      place: "Landing Hero",
      event: "Download",
      download_target: "desktop_installer",
      track_called_at: 1_700_000_000_000,
      track_delivered_at: 1_700_000_000_000,
      track_deferred: false,
    });
    expect(ctx).toEqual({ sid: "sid-1", legacy: true, conversion: true });
  });

  it("mints click_id and clicked_at and stores them for /download_success", () => {
    trackDownloadClick(CLICK, "sid-1");
    const { props } = sent();
    expect(props.click_id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    expect(typeof props.clicked_at).toBe("number");
    expect(JSON.parse(sessionStorage.getItem(DOWNLOAD_CLICK_STORAGE_KEY)!)).toEqual({
      click_id: props.click_id,
      clicked_at: props.clicked_at,
    });
  });

  it("still sends the correlation when sessionStorage throws", () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new DOMException("quota", "QuotaExceededError");
    });
    trackDownloadClick(CLICK, "sid-1");
    expect(sent().props.click_id).toEqual(expect.any(String));
  });

  it("writes no correlation when the control opts out", () => {
    sessionStorage.setItem(DOWNLOAD_CLICK_STORAGE_KEY, "previous");
    trackDownloadClick({ ...CLICK, place: "Creators Hero", download_target: "creator_hub" }, "sid-1", { correlate: false });
    const { props } = sent();
    expect(props).not.toHaveProperty("click_id");
    expect(props).not.toHaveProperty("clicked_at");
    expect(sessionStorage.getItem(DOWNLOAD_CLICK_STORAGE_KEY)).toBe("previous");
  });

  it("is not a conversion for a useTrackClick control", () => {
    trackDownloadClick(CLICK, "sid-1", { conversion: false, correlate: false });
    expect(sent().ctx).toEqual({ sid: "sid-1", legacy: true });
  });

  it("merges the URL's normalized utm_* under the control's own attributes", () => {
    window.history.replaceState(null, "", "/?utm_source=She%20Fi&utm_campaign=Launch!!&place=evil");
    trackDownloadClick(CLICK, "sid-1");
    const { props } = sent();
    expect(props).toMatchObject({ utm_source: "she_fi", utm_campaign: "launch", place: "Landing Hero" });
  });

  it("sends no utm_* without campaign parameters or with values that normalize to nothing", () => {
    window.history.replaceState(null, "", "/?utm_source=%21%21&utm_medium=");
    trackDownloadClick(CLICK, "sid-1");
    expect(Object.keys(sent().props).filter((k) => k.startsWith("utm_"))).toEqual([]);
  });

  it("adds no mac_arch off a Mac and reads no GPU", () => {
    trackDownloadClick(CLICK, "sid-1");
    expect(sent().props).not.toHaveProperty("mac_arch");
    expect(detectMacChip).not.toHaveBeenCalled();
  });

  it("adds mac_arch on a real Mac, reading the GPU once per page", () => {
    setUserAgent(MAC_UA);
    trackDownloadClick(CLICK, "sid-1");
    trackDownloadClick(CLICK, "sid-1");
    expect(vi.mocked(track).mock.calls.map(([, props]) => (props as Record<string, unknown>).mac_arch)).toEqual([
      "apple_silicon",
      "apple_silicon",
    ]);
    expect(detectMacChip).toHaveBeenCalledTimes(1);
  });

  it("adds no mac_arch for an iPad that names itself a Mac", () => {
    setUserAgent(MAC_UA, 5);
    trackDownloadClick(CLICK, "sid-1");
    expect(sent().props).not.toHaveProperty("mac_arch");
  });

  it("never throws into the click when sending fails", () => {
    vi.mocked(track).mockImplementation(() => {
      throw new Error("boom");
    });
    expect(() => trackDownloadClick(CLICK, "sid-1")).not.toThrow();
  });
});
