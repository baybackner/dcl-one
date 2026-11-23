import { afterEach, describe, expect, it, vi } from "vitest";
import type { Room, RoomEvent as RoomEventEnum } from "livekit-client";

import { RECONNECT_LIMIT_MS, STALL_MS, sessionOf } from "./session";

// The string values of livekit-client's RoomEvent members this module listens to.
const RoomEvent = {
  TrackPublished: "trackPublished",
  TrackUnpublished: "trackUnpublished",
  TrackSubscribed: "trackSubscribed",
  TrackUnsubscribed: "trackUnsubscribed",
  TrackMuted: "trackMuted",
  TrackUnmuted: "trackUnmuted",
  LocalTrackPublished: "localTrackPublished",
  LocalTrackUnpublished: "localTrackUnpublished",
  ParticipantConnected: "participantConnected",
  ParticipantDisconnected: "participantDisconnected",
  AudioPlaybackStatusChanged: "audioPlaybackChanged",
  ConnectionStateChanged: "connectionStateChanged",
  ConnectionQualityChanged: "connectionQualityChanged",
} as unknown as typeof RoomEventEnum;

type Pub = { source: string; trackSid: string; isMuted: boolean; track: object | undefined; isEnabled?: boolean; kind?: string };

function standInRoom() {
  const handlers = new Map<string, Array<(...args: unknown[]) => void>>();
  const local = { identity: "stream:p:1", connectionQuality: "excellent", trackPublications: new Map<string, Pub>() } as {
    identity: string;
    connectionQuality: string;
    trackPublications: Map<string, Pub>;
    setScreenShareEnabled: ReturnType<typeof vi.fn>;
  };
  const room = {
    canPlaybackAudio: true,
    state: "connected",
    localParticipant: local,
    remoteParticipants: new Map<string, { identity: string; trackPublications: Map<string, Pub> }>(),
    on: (ev: string, fn: (...args: unknown[]) => void) => {
      handlers.set(ev, [...(handlers.get(ev) ?? []), fn]);
      return room;
    },
    emit: (ev: string, ...args: unknown[]) => handlers.get(ev)?.forEach((fn) => fn(...args)),
    disconnect: vi.fn(async () => {}),
    startAudio: vi.fn(async () => {
      room.canPlaybackAudio = true;
    }),
  };
  local.setScreenShareEnabled = vi.fn();
  return room;
}

/** A remote camera whose decoded-frame counter the test moves. */
function remoteCamera(room: ReturnType<typeof standInRoom>, pub: Partial<Pub> = {}) {
  const media = { frames: 0 };
  const track = { getRTCStatsReport: async () => new Map([["in", { type: "inbound-rtp", framesDecoded: media.frames }]]) };
  const all: Pub = { source: "camera", trackSid: "TR_v", isMuted: false, isEnabled: true, track, ...pub };
  room.remoteParticipants.set("s", { identity: "stream:place:uuid", trackPublications: new Map([["v", all]]) });
  return media;
}

afterEach(() => {
  vi.useRealTimers();
});

describe("sessionOf", () => {
  it("counts people: itself, watchers and publishing streamers, not the bot, the ingress or a silent stream connection", () => {
    const room = standInRoom();
    const pubs = new Map([["a", { source: "camera", trackSid: "TR_a", isMuted: false, track: {} }]]);
    for (const [identity, trackPublications] of [
      ["presentation-bot:room:access:uuid", pubs],
      ["0b9d2f6e-1c57-4c1e-9a53-0d7c1c2a4e11-streamer", pubs],
      ["stream:place:access:control", new Map<string, Pub>()],
      ["watch:room:uuid", new Map<string, Pub>()],
      ["stream:place:access:speaker", pubs],
    ] as const) {
      room.remoteParticipants.set(identity, { identity, trackPublications });
    }
    expect(sessionOf(room as unknown as Room, RoomEvent).participantCount()).toBe(3);
  });

  it("is on air only while another participant has an unmuted track", () => {
    const room = standInRoom();
    const pub = (isMuted: boolean): Pub => ({ source: "camera", trackSid: "TR_a", isMuted, track: undefined });
    const s = sessionOf(room as unknown as Room, RoomEvent);
    const theirs = new Map([["a", pub(true)]]);
    room.localParticipant.trackPublications.set("a", pub(false));
    room.remoteParticipants.set("s", { identity: "stream:place:access:uuid", trackPublications: theirs });
    expect(s.onAir()).toBe(false);
    theirs.set("a", pub(false));
    expect(s.onAir()).toBe(true);
  });

  it("lists local and remote tracks with their source, mute and identity, skipping unsubscribed ones", () => {
    const room = standInRoom();
    room.localParticipant.trackPublications.set("a", { source: "camera", trackSid: "TR_a", isMuted: false, track: {} });
    room.remoteParticipants.set("w", {
      identity: "0xabc",
      trackPublications: new Map([
        ["b", { source: "microphone", trackSid: "TR_b", isMuted: true, track: {} }],
        ["c", { source: "camera", trackSid: "TR_c", isMuted: false, track: undefined }],
      ]),
    });
    const s = sessionOf(room as unknown as Room, RoomEvent);
    expect(s.tracks().map(({ key, source, muted, local }) => ({ key, source, muted, local }))).toEqual([
      { key: "stream:p:1/TR_a", source: "camera", muted: false, local: true },
      { key: "0xabc/TR_b", source: "microphone", muted: true, local: false },
    ]);
    expect(s.participantCount()).toBe(2);
    expect(s.isPublishing("camera")).toBe(true);
    expect(s.isPublishing("screen_share")).toBe(false);
  });

  it("is already disconnected when its listeners hear the room close, and a later leave does not call the room", async () => {
    const room = standInRoom();
    const s = sessionOf(room as unknown as Room, RoomEvent);
    const seen: boolean[] = [];
    s.subscribe(() => seen.push(s.disconnected()));
    room.emit("connectionStateChanged", "disconnected");
    expect(seen).toEqual([true]);
    await s.disconnect();
    expect(room.disconnect).not.toHaveBeenCalled();
  });

});

/** A remote microphone attached to one audio element. */
function remoteMicrophone(room: ReturnType<typeof standInRoom>) {
  const element = { paused: true, addEventListener: vi.fn() };
  const track = { attachedElements: [element], on: vi.fn() };
  const pub: Pub = { source: "microphone", kind: "audio", trackSid: "TR_a", isMuted: false, track };
  room.remoteParticipants.set("s", { identity: "stream:place:uuid", trackPublications: new Map([["a", pub]]) });
  return { element, track };
}

describe("sessionOf audio", () => {
  it("reports blocked audio while an audio element is paused, and unblocks it on request", async () => {
    const room = standInRoom();
    room.canPlaybackAudio = false;
    remoteMicrophone(room);
    const s = sessionOf(room as unknown as Room, RoomEvent);
    const listener = vi.fn();
    s.subscribe(listener);
    expect(s.canPlaybackAudio()).toBe(false);
    await s.startAudio();
    expect(s.canPlaybackAudio()).toBe(true);
    expect(listener).toHaveBeenCalled();
  });

  it("follows the audio elements, not the room, and tells its listeners when one starts to play", () => {
    const room = standInRoom();
    room.canPlaybackAudio = false;
    const s = sessionOf(room as unknown as Room, RoomEvent);
    expect(s.canPlaybackAudio()).toBe(true);

    const { element, track } = remoteMicrophone(room);
    room.emit("trackSubscribed", track);
    expect(track.on.mock.calls[0]![0]).toBe("elementAttached");
    track.on.mock.calls[0]![1](element);
    expect(s.canPlaybackAudio()).toBe(false);

    const listener = vi.fn();
    s.subscribe(listener);
    element.paused = false;
    expect(element.addEventListener.mock.calls[0]![0]).toBe("playing");
    element.addEventListener.mock.calls[0]![1]();
    expect(listener).toHaveBeenCalledTimes(1);
    expect(s.canPlaybackAudio()).toBe(true);
  });
});

describe("sessionOf", () => {

  it("answers false for a cancelled screen picker and true once a share publishes", async () => {
    const room = standInRoom();
    const s = sessionOf(room as unknown as Room, RoomEvent);
    room.localParticipant.setScreenShareEnabled.mockRejectedValueOnce(new Error("NotAllowedError"));
    expect(await s.setScreenShare(true)).toBe(false);
    room.localParticipant.setScreenShareEnabled.mockResolvedValueOnce({});
    expect(await s.setScreenShare(true)).toBe(true);
    room.localParticipant.setScreenShareEnabled.mockResolvedValueOnce(undefined);
    expect(await s.setScreenShare(false)).toBe(true);
  });

  it("is reconnecting while LiveKit restores the signal or the connection, and while the server lost this page's media", () => {
    const room = standInRoom();
    const s = sessionOf(room as unknown as Room, RoomEvent);
    const seen: Array<string | null> = [];
    s.subscribe(() => seen.push(s.reconnecting()));
    for (const state of ["signalReconnecting", "connected", "reconnecting", "connected"]) {
      room.emit("connectionStateChanged", state);
    }
    room.localParticipant.connectionQuality = "lost";
    room.emit("connectionQualityChanged");
    expect(seen).toEqual(["signal", null, "all", null, "all"]);
  });

  it("calls a remote track stalled when its media stops for STALL_MS, and fresh when it advances again", async () => {
    vi.useFakeTimers();
    const room = standInRoom();
    const media = remoteCamera(room);
    const s = sessionOf(room as unknown as Room, RoomEvent);
    const listener = vi.fn();
    s.subscribe(listener);
    const stalled = () => s.tracks()[0]!.stalled;

    await vi.advanceTimersByTimeAsync(STALL_MS);
    expect(stalled()).toBe(false);
    await vi.advanceTimersByTimeAsync(2_000);
    expect(stalled()).toBe(true);
    expect(listener).toHaveBeenCalledTimes(1);

    media.frames = 30;
    await vi.advanceTimersByTimeAsync(2_000);
    expect(stalled()).toBe(false);
    expect(listener).toHaveBeenCalledTimes(2);
    await s.disconnect();
  });

  it("never calls a muted track, or one the page switched off, stalled", async () => {
    vi.useFakeTimers();
    for (const pub of [{ isMuted: true }, { isEnabled: false }]) {
      const room = standInRoom();
      remoteCamera(room, pub);
      const s = sessionOf(room as unknown as Room, RoomEvent);
      await vi.advanceTimersByTimeAsync(STALL_MS * 3);
      expect(s.tracks()[0]!.stalled).toBe(false);
      await s.disconnect();
    }
  });

  it("ends a reconnection that lasts its limit, and not one that ends before", async () => {
    vi.useFakeTimers();
    for (const limit of [undefined, RECONNECT_LIMIT_MS.streamer]) {
      const ms = limit ?? RECONNECT_LIMIT_MS.watcher;
      const room = standInRoom();
      sessionOf(room as unknown as Room, RoomEvent, limit);
      room.emit("connectionStateChanged", "signalReconnecting");
      await vi.advanceTimersByTimeAsync(ms - 1);
      room.emit("connectionStateChanged", "connected");
      await vi.advanceTimersByTimeAsync(ms);
      expect(room.disconnect).not.toHaveBeenCalled();

      room.emit("connectionStateChanged", "signalReconnecting");
      await vi.advanceTimersByTimeAsync(ms / 2);
      room.emit("connectionStateChanged", "reconnecting");
      await vi.advanceTimersByTimeAsync(ms / 2);
      expect(room.disconnect).toHaveBeenCalledTimes(1);
    }
  });
});
