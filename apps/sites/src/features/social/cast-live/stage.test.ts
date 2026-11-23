import { describe, expect, it } from "vitest";

import { isCastPublisher, isPerson, pickWatcherStage, stalledCast, streamerStage, type StageCandidate } from "./stage";

const t = (identity: string, source: StageCandidate["source"], extra: Partial<StageCandidate> = {}) => ({
  identity,
  source,
  muted: false,
  local: false,
  stalled: false,
  ...extra,
});

describe("pickWatcherStage", () => {
  it("prefers a Cast screen share, then any screen share, then a Cast camera, then any camera", () => {
    const wallet = "0xabc";
    const streamer = "stream:place-1:uuid";
    const bot = "presentation-bot:room:uuid";
    expect(pickWatcherStage([t(wallet, "camera"), t(streamer, "camera")])?.identity).toBe(streamer);
    expect(pickWatcherStage([t(streamer, "camera"), t(wallet, "screen_share")])?.identity).toBe(wallet);
    expect(
      pickWatcherStage([t(wallet, "screen_share"), t(bot, "screen_share"), t(streamer, "camera")])?.identity,
    ).toBe(bot);
    expect(pickWatcherStage([t(wallet, "camera")])?.identity).toBe(wallet);
  });

  it("shows a track that advances before a stalled one, and a stalled one when nothing else is there", () => {
    const slide = t("presentation-bot:room:uuid", "screen_share", { stalled: true });
    expect(pickWatcherStage([slide, t("stream:place-1:uuid", "camera")])?.source).toBe("camera");
    expect(pickWatcherStage([slide])).toBe(slide);
  });

  it("never shows a muted track, audio, or nothing at all", () => {
    expect(pickWatcherStage([])).toBeNull();
    expect(pickWatcherStage([t("stream:a:b", "microphone"), t("stream:a:b", "screen_share_audio")])).toBeNull();
    expect(
      pickWatcherStage([t("stream:a:b", "screen_share", { muted: true }), t("0x1", "camera")])?.identity,
    ).toBe("0x1");
  });

  it("keeps the first of equal rank", () => {
    const a = t("stream:p:1", "screen_share");
    const b = t("stream:p:2", "screen_share");
    expect(pickWatcherStage([a, b])).toBe(a);
  });
});

describe("streamerStage", () => {
  it("shows only the streamer's own unmuted screen then camera", () => {
    const cam = t("stream:p:1", "camera", { local: true });
    const screen = t("stream:p:1", "screen_share", { local: true });
    const remote = t("0x1", "screen_share");
    const mutedMic = t("stream:p:1", "microphone", { local: true });
    expect(streamerStage([cam, remote, screen, mutedMic])).toEqual([screen, cam]);
    expect(streamerStage([{ ...cam, muted: true }])).toEqual([]);
  });
});

describe("isCastPublisher", () => {
  it("names the gatekeeper's streamer and presentation-bot identities only", () => {
    expect(isCastPublisher("stream:place:uuid")).toBe(true);
    expect(isCastPublisher("presentation-bot:room:uuid")).toBe(true);
    expect(isCastPublisher("watch:room:uuid")).toBe(false);
    expect(isCastPublisher("0xabc")).toBe(false);
  });
});

describe("isPerson", () => {
  it("counts a watcher, a wallet in the scene and a streamer that publishes", () => {
    expect(isPerson("watch:room:uuid", false)).toBe(true);
    expect(isPerson("0xabc", false)).toBe(true);
    expect(isPerson("stream:place:access:uuid", true)).toBe(true);
  });

  it("never counts the presentation bot, the RTMP ingress or a stream key's connection that publishes nothing", () => {
    expect(isPerson("presentation-bot:room:access:uuid", true)).toBe(false);
    expect(isPerson("0b9d2f6e-1c57-4c1e-9a53-0d7c1c2a4e11-streamer", true)).toBe(false);
    expect(isPerson("stream:place:access:uuid", false)).toBe(false);
  });
});

describe("stalledCast", () => {
  const streamer = "stream:place-1:uuid";
  const frozen = { stalled: true };

  it("is true when the picture a watcher is shown does not advance, whatever the sound does", () => {
    expect(stalledCast([t(streamer, "camera", frozen), t(streamer, "microphone")])).toBe(true);
    expect(stalledCast([t(streamer, "camera"), t(streamer, "microphone", frozen)])).toBe(false);
  });

  it("with no picture, is true when no sound advances", () => {
    expect(stalledCast([t(streamer, "microphone", frozen)])).toBe(true);
    expect(stalledCast([t(streamer, "microphone", frozen), t("0x1", "microphone")])).toBe(false);
  });

  it("is false with nothing on air, and never reads the page's own tracks", () => {
    expect(stalledCast([])).toBe(false);
    expect(stalledCast([t(streamer, "camera", { local: true, stalled: true })])).toBe(false);
  });
});
