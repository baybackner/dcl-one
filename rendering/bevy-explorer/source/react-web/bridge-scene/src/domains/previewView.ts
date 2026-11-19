// The avatar preview's orthographic view, as plain arithmetic: how far a zoomed view may move inside its framing,
// and where a point by the avatar falls in the preview's window.

/** The visible height of the view (metres, orthographic) and the height at the avatar it is centred on. */
export type Framing = { range: number; centerY: number }
/** Metres along the scene's axes: x east, y up, z north. */
export type SceneOffset = { x: number; y: number; z: number }
/** Shares of the preview window: of its width from its left, of its height from its top. */
export type WindowPoint = { x: number; y: number }

/** The view's centre moved `shift` metres up at zoom `z`, kept inside the framing. */
export function shiftWithin(f: { range: number }, z: number, shift: number): number {
  const room = (f.range - f.range / z) / 2
  return Math.max(-room, Math.min(room, shift))
}

/** Where a point by the avatar falls in the preview's window. `offset` is from the avatar's feet, which stand at
 *  `feetY`, centred across the view. The camera looks north, orthographic and pitched `pitchDeg` down, placed so its
 *  view centre stands at the framing's height at the avatar. So a point shows across at its distance east over the
 *  view's width (its height times the window's `aspect`, width over height), and up at its height above that
 *  centre foreshortened by the pitch, raised by the pitch for its depth beyond the avatar, over the view's height. */
export function windowPoint(offset: SceneOffset, f: Framing, z: number, shift: number, pitchDeg: number, aspect: number, feetY = 0): WindowPoint {
  const pitch = (pitchDeg * Math.PI) / 180
  const height = f.range / z
  const centre = f.centerY + shiftWithin(f, z, shift)
  const up = (feetY + offset.y - centre) * Math.cos(pitch) + offset.z * Math.sin(pitch)
  return { x: 0.5 + offset.x / (height * aspect), y: 0.5 - up / height }
}
