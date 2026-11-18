// Where the Shop's avatar has its nametag (previewNametag, scene → page): the session keeps the engine's last word
// on it, and forgets it once the engine says there is none.
import { afterEach, describe, expect, it } from 'vitest'
import { act, cleanup, waitFor } from '@testing-library/react'
import { renderSession } from '../harness'

afterEach(() => {
  cleanup()
})

describe("the Shop avatar's nametag", () => {
  it('stands where the engine last said, and is unknown once the engine has none', async () => {
    const h = renderSession({ userId: null })
    expect(h.session().shop.nametagTop ?? null).toBeNull()
    act(() => h.driver.emit({ kind: 'previewNametag', top: 0.12 }))
    await waitFor(() => expect(h.session().shop.nametagTop).toBe(0.12))
    act(() => h.driver.emit({ kind: 'previewNametag', top: null }))
    await waitFor(() => expect(h.session().shop.nametagTop).toBeNull())
  })
})
