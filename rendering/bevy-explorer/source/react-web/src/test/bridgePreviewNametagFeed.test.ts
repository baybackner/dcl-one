// The Shop's nametag, as the bridge tells it to the page (bridge-scene/src/domains/previewNametagFeed): read while
// the stage is up and told once it moves; none when the stage comes up again or goes, the avatar goes, or a read
// fails; and an answer from before never lands on the stage after.
import { describe, expect, it } from 'vitest'
import { createNametagFeed, NAMETAG_READ_SECONDS } from '../../bridge-scene/src/domains/previewNametagFeed'

type Point = { x: number; y: number }

/** A feed whose reads wait to be answered, in the order they were made; the anchor is the point itself. */
function feed() {
  const reads: { avatar: number; answer: (p: Point | null) => void; fail: () => void }[] = []
  const sent: (Point | null)[] = []
  const nametag = createNametagFeed<number, Point>({
    read: (avatar) =>
      new Promise<Point | null>((resolve, reject) => {
        reads.push({ avatar, answer: resolve, fail: () => reject(new Error('read failed')) })
      }),
    project: (p) => p,
    send: (p) => sent.push(p)
  })
  return { nametag, reads, sent }
}

const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0))
const A = { x: 0.53, y: 0.12 }
const B = { x: 0.47, y: 0.31 }

describe("the Shop's nametag feed", () => {
  it('tells the page where the anchor is, and that there is none once the stage goes', async () => {
    const { nametag, reads, sent } = feed()
    nametag.tick(0, 7)
    reads[0].answer(A)
    await settle()
    expect(sent).toEqual([A])
    nametag.restage() // the stage goes
    nametag.restage()
    expect(sent).toEqual([A, null])
  })

  it('drops an answer begun before the stage came up again, for the same avatar', async () => {
    const { nametag, reads, sent } = feed()
    nametag.tick(0, 7)
    reads[0].answer(A)
    await settle()
    nametag.tick(NAMETAG_READ_SECONDS, 7) // a second read, pending
    expect(reads).toHaveLength(2)
    nametag.restage() // closed
    nametag.restage() // and opened again
    reads[1].answer(B) // the old stage's answer
    await settle()
    expect(sent).toEqual([A, null])
    nametag.tick(0, 7)
    reads[2].answer(B)
    await settle()
    expect(sent).toEqual([A, null, B])
  })

  it('goes back to the design while reads fail, and to the anchor once one answers', async () => {
    const { nametag, reads, sent } = feed()
    nametag.tick(0, 7)
    reads[0].answer(A)
    await settle()
    nametag.tick(NAMETAG_READ_SECONDS, 7)
    reads[1].fail()
    await settle()
    nametag.tick(NAMETAG_READ_SECONDS, 7)
    reads[2].answer(A)
    await settle()
    expect(sent).toEqual([A, null, A])
  })

  it("tells a move only once it passes a pixel, and drops another avatar's answer", async () => {
    const { nametag, reads, sent } = feed()
    nametag.tick(0, 7)
    reads[0].answer(A)
    await settle()
    nametag.tick(NAMETAG_READ_SECONDS, 7)
    reads[1].answer({ x: A.x, y: A.y + 0.0004 })
    await settle()
    expect(sent).toEqual([A])
    nametag.tick(NAMETAG_READ_SECONDS, 7) // pending for avatar 7...
    nametag.tick(0, 8) // ...when avatar 8 takes the stage
    reads[2].answer(B)
    await settle()
    expect(reads.map((r) => r.avatar)).toEqual([7, 7, 7, 8])
    expect(sent).toEqual([A, null])
  })

  it("leaves the last avatar's place as soon as another takes the stage, until the new one's read answers", async () => {
    const { nametag, reads, sent } = feed()
    nametag.tick(0, 7)
    reads[0].answer(A)
    await settle()
    nametag.tick(0, 8) // avatar 8's read is held
    expect(sent).toEqual([A, null])
    reads[1].answer(B)
    await settle()
    expect(sent).toEqual([A, null, B])
  })

  it('tells the page there is none once the avatar goes, and reads only every so often', async () => {
    const { nametag, reads, sent } = feed()
    nametag.tick(0, 7)
    reads[0].answer(A)
    await settle()
    nametag.tick(NAMETAG_READ_SECONDS / 2, 7)
    expect(reads).toHaveLength(1)
    nametag.tick(0, null)
    expect(sent).toEqual([A, null])
  })

  it('follows the view as it moves, from the last anchor read and without reading again, until the stage goes', async () => {
    const reads: ((p: Point | null) => void)[] = []
    const sent: (Point | null)[] = []
    let lift = 0 // the view's move: a point shows this much higher in the window
    const nametag = createNametagFeed<number, Point>({
      read: () => new Promise<Point | null>((resolve) => reads.push(resolve)),
      project: (p) => ({ x: p.x, y: p.y - lift }),
      send: (p) => sent.push(p)
    })
    nametag.reproject() // nothing read yet: nothing to tell
    nametag.tick(0, 7)
    reads[0](A)
    await settle()
    lift = 0.05 // the camera eases to Names' framing
    nametag.reproject()
    expect(sent).toEqual([A, { x: A.x, y: A.y - 0.05 }])
    expect(reads).toHaveLength(1)
    nametag.restage() // the stage goes
    nametag.reproject()
    expect(sent).toEqual([A, { x: A.x, y: A.y - 0.05 }, null])
  })
})
