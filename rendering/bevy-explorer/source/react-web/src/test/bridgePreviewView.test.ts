// The avatar preview's view arithmetic (bridge-scene/src/domains/previewView): where a height at the avatar falls in
// the preview's window, for the Shop's nametag, and how far a zoomed view moves.
import { describe, expect, it } from 'vitest'
import { shareFromTop, shiftWithin } from '../../bridge-scene/src/domains/previewView'

const SHOP = { range: 4.9, centerY: 1.84 }
const PITCH = 12
const COS = Math.cos((PITCH * Math.PI) / 180)

describe("the avatar preview's view", () => {
  it('shows the framing height at the middle of the window, and its edges at the top and the bottom', () => {
    expect(shareFromTop(SHOP.centerY, SHOP, 1, 0, PITCH)).toBeCloseTo(0.5, 9)
    expect(shareFromTop(SHOP.centerY + SHOP.range / 2 / COS, SHOP, 1, 0, PITCH)).toBeCloseTo(0, 9)
    expect(shareFromTop(SHOP.centerY - SHOP.range / 2 / COS, SHOP, 1, 0, PITCH)).toBeCloseTo(1, 9)
  })

  it("puts the Shop's feet near the floor of its 942px window, as the design stands the avatar", () => {
    // 0.5 + 1.84 × cos 12° / 4.9: about 817px down the window
    expect(shareFromTop(0, SHOP, 1, 0, PITCH) * 942).toBeCloseTo(817, 0)
  })

  it('zooms about the moved centre, which stays inside the framing', () => {
    expect(shiftWithin(SHOP, 2, 10)).toBeCloseTo((SHOP.range - SHOP.range / 2) / 2, 9)
    expect(shiftWithin(SHOP, 1, 10)).toBe(0)
    // twice the zoom, half the view: a height 1m over the centre moves twice as far from the middle
    const once = shareFromTop(SHOP.centerY + 1, SHOP, 1, 0, PITCH)
    const twice = shareFromTop(SHOP.centerY + 1, SHOP, 2, 0, PITCH)
    expect(0.5 - twice).toBeCloseTo(2 * (0.5 - once), 9)
  })
})
