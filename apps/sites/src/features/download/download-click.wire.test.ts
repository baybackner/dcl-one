// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { dropTrackedUser, restoreTrackedUser } from "@/core/lib/telemetry/tracked-user";

import { trackDownloadClick } from "./download-click";

// The serialized Segment body a download Click leaves the browser with, through the real transport.

const BROWSER_UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const UA_DATA = { brands: [{ brand: "Chromium", version: "131" }], mobile: false, platform: "Windows", architecture: "x86" };

const sendBeacon = vi.fn((_url: string, _body: Blob) => true);

function browser(userAgentData?: unknown) {
  vi.stubGlobal("navigator", {
    userAgent: BROWSER_UA,
    language: "en-US",
    maxTouchPoints: 0,
    sendBeacon,
    ...(userAgentData ? { userAgentData } : {}),
  });
}

async function body(): Promise<Record<string, unknown> & { context: Record<string, unknown> }> {
  expect(sendBeacon).toHaveBeenCalledTimes(1);
  return JSON.parse(await sendBeacon.mock.calls[0]![1].text());
}

beforeEach(() => {
  (window as unknown as { __DCL_PUBLIC__: Record<string, string> }).__DCL_PUBLIC__ = { SEGMENT_WRITE_KEY: "key-test" };
  browser();
});
afterEach(() => {
  vi.unstubAllGlobals();
  sendBeacon.mockClear();
  restoreTrackedUser();
  delete (window as unknown as { __DCL_PUBLIC__?: unknown }).__DCL_PUBLIC__;
});

const CLICK = { place: "Landing Hero", event: "Download", download_target: "desktop_installer" } as const;

it("names a signed-in visitor by the verified wallet, lowercased", async () => {
  trackDownloadClick(CLICK, { sid: "sid-1", userId: "0xAbC" });
  const sent = await body();
  expect(sent).toMatchObject({ type: "track", event: "Click", anonymousId: "sid-1", userId: "0xabc" });
  expect(sent.properties).toMatchObject({ place: "Landing Hero", event: "Download", download_target: "desktop_installer" });
});

it("sends no userId for an anonymous visitor", async () => {
  trackDownloadClick(CLICK, { sid: "sid-1" });
  expect(await body()).not.toHaveProperty("userId");
});

it("sends no userId once a sign-out has started", async () => {
  dropTrackedUser();
  trackDownloadClick(CLICK, { sid: "sid-1", userId: "0xabc" });
  expect(await body()).not.toHaveProperty("userId");
});

it("carries the low-entropy userAgentData when the browser exposes it", async () => {
  browser(UA_DATA);
  trackDownloadClick(CLICK, { sid: "sid-1" });
  expect((await body()).context.userAgentData).toEqual({
    brands: [{ brand: "Chromium", version: "131" }],
    mobile: false,
    platform: "Windows",
  });
});

it("leaves userAgentData out where the browser has none", async () => {
  trackDownloadClick(CLICK, { sid: "sid-1" });
  expect((await body()).context).not.toHaveProperty("userAgentData");
});
