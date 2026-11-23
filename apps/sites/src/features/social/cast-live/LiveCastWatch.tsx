import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { useAuth } from "@/data/lib/auth/context";
import type { AuthIdentity } from "@/data/lib/auth/types";
import { getWatcherToken, defaultDisplayName, type CastCredentials } from "@/data/lib/catalyst/cast";
import { CatalystError } from "@/data/lib/catalyst/client";
import {
  fallbackPlaceName,
  isWorldLocation,
  type WatchResult,
} from "@/data/lib/catalyst/landings/cast-watcher";
import type { TrackContext } from "@/core/lib/telemetry/track";

import { openSignIn } from "../../components/auth/signin-store";
import { JOIN_TIMEOUT_MS, NO_ANSWER } from "../cast-stream/machine";
import CastWatch, { type WatchJoin, type WatchLive, type WatchRefusal } from "../cast-watch/CastWatch";
import { WatcherStage, useCastTracks } from "./CastStage";
import { joinCast, type CastSession } from "./session";
import { stalledCast } from "./stage";

/** How the gatekeeper's refusals of `POST /cast/watcher-token` read on the page. */
export function watchRefusal(err: unknown): WatchRefusal {
  const status = err instanceof CatalystError ? err.status : 0;
  const server = err instanceof CatalystError && err.serverMessage ? err.message : null;
  if (status === 404) {
    return {
      kind: "waiting",
      title: "No one is currently casting to this link...",
      message: "This Decentraland Cast may be starting soon or has already ended.",
    };
  }
  if (status === 401 && server === "Stream access has expired") {
    return {
      kind: "waiting",
      title: "This Cast has ended",
      message: "The stream link for this place has expired.",
    };
  }
  if (status === 403) {
    return { kind: "error", title: "You can't watch this Cast", message: server ?? "Access denied." };
  }
  return {
    kind: "error",
    title: "Could not join the cast",
    message: err instanceof Error ? err.message : "Try again in a moment.",
  };
}

export type WatchDeps = {
  getToken: typeof getWatcherToken;
  join: (credentials: CastCredentials, opts: { signal?: AbortSignal }) => Promise<CastSession>;
};

const LIVE_DEPS: WatchDeps = { getToken: getWatcherToken, join: (c, o) => joinCast(c, o) };

/** `run`'s answer, or a rejection with `NO_ANSWER` when it has none in `ms`: its signal aborts
 * then, as it does with `signal`. */
export function answeredIn<T>(ms: number, signal: AbortSignal, run: (signal: AbortSignal) => Promise<T>): Promise<T> {
  const timeout = AbortSignal.timeout(ms);
  const late = new Promise<never>((_, reject) =>
    timeout.addEventListener("abort", () => reject(new Error(NO_ANSWER))),
  );
  return Promise.race([run(AbortSignal.any([signal, timeout])), late]);
}

/** One watcher join: the signed token, then the room. A join the page abandoned (`signal`)
 * disconnects whatever it connected and answers null. */
export async function joinAsWatcher(
  identity: AuthIdentity,
  where: { location: string; parcel?: string },
  signal: AbortSignal,
  deps: WatchDeps = LIVE_DEPS,
): Promise<{ refused: WatchRefusal } | { credentials: CastCredentials; session: CastSession } | null> {
  let credentials: CastCredentials;
  try {
    credentials = await deps.getToken(
      identity,
      { ...where, displayName: defaultDisplayName(identity.signer) },
      { signal },
    );
  } catch (err) {
    if (signal.aborted) return null;
    return { refused: watchRefusal(err) };
  }
  if (signal.aborted) return null;
  let session: CastSession;
  try {
    session = await deps.join(credentials, { signal });
  } catch (err) {
    if (signal.aborted) return null;
    throw err;
  }
  if (signal.aborted) {
    await session.disconnect();
    return null;
  }
  return { credentials, session };
}

/** `/cast/w/{location}`: watches the Cast of a parcel or a world with the viewer's own wallet. */
export default function LiveCastWatch({
  trackCtx,
  location,
  parcel,
}: {
  trackCtx: TrackContext;
  location: string;
  parcel?: string;
}) {
  const { identity } = useAuth();
  const [session, setSession] = useState<CastSession | null>(null);
  const sessionRef = useRef<CastSession | null>(null);
  const aborter = useRef<AbortController | null>(null);
  const tracks = useCastTracks(session);
  const [placeName, setPlaceName] = useState(() => fallbackPlaceName(location));

  useEffect(
    () => () => {
      aborter.current?.abort();
      void sessionRef.current?.disconnect();
      sessionRef.current = null;
    },
    [],
  );

  const join = useCallback(async (): Promise<WatchJoin> => {
    if (!identity) {
      openSignIn();
      return {
        refused: {
          kind: "error",
          title: "Sign in to watch",
          message: "Watching a Cast needs your Decentraland account.",
        },
      };
    }
    aborter.current?.abort();
    const controller = new AbortController();
    aborter.current = controller;
    const joined = await answeredIn(JOIN_TIMEOUT_MS, controller.signal, (signal) =>
      joinAsWatcher(identity, { location, parcel }, signal),
    );
    // Abandoned: the page left or a newer join started. Never settles, so nothing renders.
    if (joined === null) return new Promise<WatchJoin>(() => {});
    if ("refused" in joined) return { refused: joined.refused };
    if (joined.credentials.placeName) setPlaceName(joined.credentials.placeName);
    sessionRef.current = joined.session;
    setSession(joined.session);
    return { roomId: joined.credentials.roomId };
  }, [identity, location, parcel]);

  const leave = useCallback(() => {
    aborter.current?.abort();
    const s = sessionRef.current;
    sessionRef.current = null;
    setSession(null);
    void s?.disconnect();
  }, []);

  const bindLost = useCallback(
    (onLost: () => void) => {
      if (!session) return () => {};
      return session.subscribe(() => {
        // Our own leave clears the ref first; anything else is a lost room.
        if (session.disconnected() && sessionRef.current === session) {
          sessionRef.current = null;
          setSession(null);
          onLost();
        }
      });
    },
    [session],
  );

  const audioBlocked = session ? !session.canPlaybackAudio() : false;
  // A whole reconnection empties the room, so it is asked before what is on air.
  const onAir = session?.onAir();
  const away = session?.reconnecting() === "all" || (onAir && stalledCast(tracks));
  const signal = away ? "reconnecting" : onAir ? "live" : "none";
  const live = useMemo<WatchLive>(
    () => ({
      join,
      leave,
      bindLost,
      stage: (muted) => <WatcherStage tracks={tracks} muted={muted} />,
      participantCount: session ? session.participantCount() : 0,
      signal,
      audioBlocked,
      enableAudio: () => void session?.startAudio(),
    }),
    [join, leave, bindLost, tracks, session, audioBlocked, signal],
  );

  const watch: WatchResult = {
    status: "live",
    location,
    isWorld: isWorldLocation(location),
    placeName,
    identity: identity ? defaultDisplayName(identity.signer) : "Viewer",
  };

  return <CastWatch trackCtx={trackCtx} watch={watch} live={live} />;
}
