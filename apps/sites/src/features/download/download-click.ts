import { collectCampaignParams, type CampaignParams } from "@/core/lib/telemetry/campaign-params";
import { track } from "@/core/lib/telemetry/track";
import { detectMacChip, isTouchMac, type MacChip } from "@/data/lib/download/mac-chip";

// The legacy site's `Click` for a download call to action (decentraland/sites `useDownloadClick` and
// `useTrackClick` over `buildClickPayload`). Warehouse models read these rows by exact `place` and
// `download_target` values, so every name and value below is the legacy one, byte for byte.

export type DownloadClickPlace =
  | "Landing Hero"
  | "landing-hero-epic"
  | "landing-hero-platform-switch"
  | "Landing Come Hang Out"
  | "come-hang-out"
  | "come-hang-out-platform-switch"
  | "Download"
  | "download-page"
  | "Creators Hero"
  | "Creator Hub Success Footer";

export type DownloadTarget = "desktop_installer" | "epic" | "app_store" | "google_play" | "creator_hub";

export type DownloadClickOs = "Android" | "iOS" | "Windows" | "macOS";

/** The control's own attributes: legacy `data-place`, `data-event` (absent where it was `Click`), `data-os`. */
export type DownloadClick = {
  place: DownloadClickPlace;
  event?: "Download";
  download_target: DownloadTarget;
  os?: DownloadClickOs;
};

type DownloadClickProps = CampaignParams &
  DownloadClick & {
    click_id?: string;
    clicked_at?: number;
    mac_arch?: MacChip;
    track_called_at: number;
    track_delivered_at: number;
    track_deferred: boolean;
  };

export type DownloadClickOptions = {
  /**
   * A `useDownloadClick` control (the default) is a conversion: sent on an exempt session and for a bot, as the
   * legacy beacon did. A `useTrackClick` control (the Creator Hub download page and its success page) is not.
   */
  conversion?: boolean;
  /**
   * Mints the `click_id`/`clicked_at` pair the Explorer funnel on /download_success joins on. Off for the
   * Creator Hub, whose funnel joins on the anonymous id.
   */
  correlate?: boolean;
};

export const DOWNLOAD_CLICK_STORAGE_KEY = "downloadFunnel:lastClick";

type Correlation = { click_id: string; clicked_at: number };

function uuid(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") return crypto.randomUUID();
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (char) => {
    const random = Math.floor(Math.random() * 16);
    return (char === "x" ? random : (random & 0x3) | 0x8).toString(16);
  });
}

/** Stored for /download_success; blocked storage loses only that join, never the click. */
function recordCorrelation(): Correlation {
  const correlation = { click_id: uuid(), clicked_at: Date.now() };
  try {
    sessionStorage.setItem(DOWNLOAD_CLICK_STORAGE_KEY, JSON.stringify(correlation));
  } catch {
    // Private mode or a full quota.
  }
  return correlation;
}

// The GPU cannot change during a page load: one WebGL read at most.
let macArch: MacChip | null | undefined;

/** Real Macs only: an iPhone never says "Macintosh", and an iPad that does has a touch screen. */
function macArchHint(): MacChip | null {
  if (macArch === undefined) {
    macArch = /Macintosh/.test(navigator.userAgent) && !isTouchMac() ? detectMacChip() : null;
  }
  return macArch;
}

/** Test hook: forget the memoized chip so a test can change the User-Agent. */
export function resetMacArchHint(): void {
  macArch = undefined;
}

/**
 * Sends the legacy `Click` for a download control. Merge order is the legacy one: the URL's `utm_*`, then the
 * correlation, then the control's own attributes.
 */
export function trackDownloadClick(
  click: DownloadClick,
  sid: string,
  { conversion = true, correlate = true }: DownloadClickOptions = {},
): void {
  try {
    const calledAt = Date.now();
    const arch = macArchHint();
    const props: DownloadClickProps = {
      ...collectCampaignParams(),
      ...(correlate ? recordCorrelation() : {}),
      ...click,
      ...(arch ? { mac_arch: arch } : {}),
      // Sent at once: nothing waits for an SDK to load, so the click is never deferred.
      track_called_at: calledAt,
      track_delivered_at: Date.now(),
      track_deferred: false,
    };
    track("Click", props, { sid, legacy: true, ...(conversion ? { conversion: true } : {}) });
  } catch {
    // Telemetry never breaks a download.
  }
}
