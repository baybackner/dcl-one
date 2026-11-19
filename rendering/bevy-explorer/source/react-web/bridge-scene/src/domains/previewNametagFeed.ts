// The Shop's nametag over its avatar, told to the page (previewNametag): where the engine has the avatar's nametag
// anchor, read now and then while the Shop's stage is up and told once it moves; or none, for the page to keep the
// design's place, when the stage comes up again or goes, the avatar goes or changes, or a read fails. A read begun
// before the stage came up again, or for another avatar, never lands.

import type { WindowPoint } from './previewView'

/** The least move told to the page: about a pixel of the 942px window. */
export const NAMETAG_STEP = 0.001
/** How often the anchor is read while the stage is up. */
export const NAMETAG_READ_SECONDS = 0.25

export interface NametagFeed<A> {
  /** The Shop's stage came up or went: a page holding a place is told there is none, and reads begun before are
   *  dropped. */
  restage(): void
  /** A frame, `dt` seconds on: `avatar` is the Shop's avatar while its stage is up, else null. */
  tick(dt: number, avatar: A | null): void
  /** The view moved (the camera easing to another page's framing): the last anchor read is told where it falls
   *  now, without waiting for the next read. */
  reproject(): void
}

export function createNametagFeed<A, O>(deps: {
  /** Where `avatar` has its nametag anchor now; null while the engine has none. */
  read: (avatar: A) => Promise<O | null>
  /** The anchor in the window, by the view as it is when the answer lands. */
  project: (anchor: O) => WindowPoint
  send: (anchor: WindowPoint | null) => void
  /** The anchor kept from a read, given the one kept before (none since the last drop); the read itself without it. */
  settle?: (held: O | null, read: O) => O
}): NametagFeed<A> {
  let told: WindowPoint | null = null
  // The last anchor kept for the avatar on the stage: none since the last drop.
  let last: O | null = null
  let generation = 0
  let reading = false
  let wait = 0
  let lastAvatar: A | null = null
  const publish = (anchor: WindowPoint | null): void => {
    if (anchor == null || !Number.isFinite(anchor.x) || !Number.isFinite(anchor.y)) {
      if (told != null) {
        told = null
        deps.send(null)
      }
      return
    }
    if (told != null && Math.abs(anchor.x - told.x) < NAMETAG_STEP && Math.abs(anchor.y - told.y) < NAMETAG_STEP) return
    told = anchor
    deps.send(anchor)
  }
  const drop = (): void => {
    generation++
    reading = false
    wait = 0
    last = null
  }
  return {
    restage(): void {
      drop()
      lastAvatar = null
      publish(null)
    },
    tick(dt: number, avatar: A | null): void {
      if (avatar == null) {
        if (lastAvatar != null) drop()
        lastAvatar = null
        publish(null)
        return
      }
      if (avatar !== lastAvatar) {
        // another avatar: the last one's place isn't its, until its own read answers
        drop()
        lastAvatar = avatar
        publish(null)
      }
      wait -= dt
      if (reading || wait > 0) return
      wait = NAMETAG_READ_SECONDS
      reading = true
      const asked = generation
      deps.read(avatar).then(
        (anchor) => {
          if (asked !== generation) return
          reading = false
          last = anchor == null ? null : deps.settle != null ? deps.settle(last, anchor) : anchor
          publish(last == null ? null : deps.project(last))
        },
        () => {
          // a failed read: the design's place until a read answers again
          if (asked !== generation) return
          reading = false
          last = null
          publish(null)
        }
      )
    },
    reproject(): void {
      if (last != null) publish(deps.project(last))
    }
  }
}
