// Which video a Cast page shows. Pure, so it is tested without LiveKit.

/** LiveKit's `Track.Source` values a Cast room carries. */
export type CastSource = "camera" | "microphone" | "screen_share" | "screen_share_audio" | "unknown";

export type StageCandidate = {
  /** The participant's LiveKit identity (`stream:…`, `presentation-bot:…`, a wallet). */
  identity: string;
  source: CastSource;
  muted: boolean;
  local: boolean;
  /** Its media stopped arriving (`STALL_MS` in `session.ts`). */
  stalled: boolean;
};

/** The identities the gatekeeper mints for Cast publishers (`stream:<place>:<uuid>`,
 * `presentation-bot:<room>:<uuid>`), as the Bevy explorer classifies them. */
export function isCastPublisher(identity: string): boolean {
  return identity.startsWith("stream:") || identity.startsWith("presentation-bot:");
}

/** Who the people badge counts: a watcher (`watch:…`), a wallet in the scene and a streamer
 * (`stream:…`) that publishes. Never the machines the gatekeeper lets in: the presentation bot,
 * the RTMP ingress (`<uuid>-streamer`) and a stream key's connection that publishes nothing. */
export function isPerson(identity: string, publishes: boolean): boolean {
  if (identity.startsWith("presentation-bot:") || identity.endsWith("-streamer")) return false;
  return publishes || !identity.startsWith("stream:");
}

function rank(c: StageCandidate): number {
  const screen = c.source === "screen_share";
  const cast = isCastPublisher(c.identity);
  // A stalled track ranks after every track that advances.
  return (c.stalled ? 4 : 0) + (screen && cast ? 0 : screen ? 1 : cast ? 2 : 3);
}

/** The video a watcher sees: a track that advances before a stalled one (a slide whose bot died
 * gives way to the camera), a screen share before a camera, a Cast publisher before anyone else
 * in the scene room, and never a muted or non-video track. Ties keep the input order. */
export function pickWatcherStage<T extends StageCandidate>(tracks: readonly T[]): T | null {
  let best: T | null = null;
  for (const t of tracks) {
    if (t.muted || (t.source !== "camera" && t.source !== "screen_share")) continue;
    if (best === null || rank(t) < rank(best)) best = t;
  }
  return best;
}

/** What a streamer sees of themself: the screen they share, then their camera. */
export function streamerStage<T extends StageCandidate>(tracks: readonly T[]): T[] {
  const own = tracks.filter((t) => t.local && !t.muted);
  return [
    ...own.filter((t) => t.source === "screen_share"),
    ...own.filter((t) => t.source === "camera"),
  ];
}

/** A Cast on air does not arrive: the picture a watcher is shown stands still or, with no
 * picture, every sound does. */
export function stalledCast(tracks: readonly StageCandidate[]): boolean {
  const theirs = tracks.filter((t) => !t.local && !t.muted);
  const shown = pickWatcherStage(theirs);
  return shown ? shown.stalled : theirs.length > 0 && theirs.every((t) => t.stalled);
}
