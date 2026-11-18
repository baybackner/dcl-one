// The avatar preview's orthographic view, as plain arithmetic: how far a zoomed view may move inside its framing,
// and where a height at the avatar falls in the preview's window.

/** The visible height of the view (metres, orthographic) and the height at the avatar it is centred on. */
export type Framing = { range: number; centerY: number }

/** The view's centre moved `shift` metres up at zoom `z`, kept inside the framing. */
export function shiftWithin(f: { range: number }, z: number, shift: number): number {
  const room = (f.range - f.range / z) / 2
  return Math.max(-room, Math.min(room, shift))
}

/** Where a height at the avatar (metres, in the scene's coordinates) falls in the preview's window, as a share of the
 *  window's height from its top. The camera is orthographic, pitched `pitchDeg` down and placed so that its view
 *  centre stands at the framing's height at the avatar: a height there shows at its distance from that centre,
 *  foreshortened by the pitch, over the view's height. */
export function shareFromTop(y: number, f: Framing, z: number, shift: number, pitchDeg: number): number {
  const centre = f.centerY + shiftWithin(f, z, shift)
  return 0.5 - ((y - centre) * Math.cos((pitchDeg * Math.PI) / 180)) / (f.range / z)
}
