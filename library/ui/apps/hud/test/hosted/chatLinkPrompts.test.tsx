import { describe, it, expect, vi, afterEach, beforeEach } from 'vitest'
import { render, screen, act } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { PopupHost, ToastHost, closeTopPopup, resetPopups } from '../../design'
import { confirmOpenLink, confirmTeleport } from '../../features/chat/chatLinkPrompts'
import { fetchPlaceAt } from '@explorer-core/features/map/mapApi'

const host = vi.hoisted(() => ({ native: false }))
vi.mock('@hud-host', async (importOriginal) => ({
  ...(await importOriginal<object>()),
  DEFAULT_REALM: 'https://realm.example/main',
  isNativeHud: () => host.native
}))
vi.mock('@explorer-core/features/map/mapApi', () => ({
  fetchPlaceAt: vi.fn(async () => ({ id: 'p1', title: 'Genesis Plaza' })),
  isEmptyPlace: () => false
}))

beforeEach(() => {
  host.native = false
  render(
    <>
      <PopupHost />
      <ToastHost />
    </>
  )
})
afterEach(() => {
  resetPopups()
  vi.restoreAllMocks()
  vi.mocked(fetchPlaceAt).mockClear()
})

function start<T>(run: () => Promise<T>): Promise<T> {
  let done!: Promise<T>
  act(() => {
    done = run()
  })
  return done
}

describe('teleport confirmation', () => {
  it('teleports only after Jump in, naming the Genesis City place', async () => {
    const teleport = vi.fn()
    const done = start(() => confirmTeleport(52, 2, 'https://realm.example/main', teleport))
    expect(await screen.findByText('Genesis Plaza')).toBeTruthy()
    expect(fetchPlaceAt).toHaveBeenCalledWith({ x: 52, y: 2 }, undefined)
    expect(teleport).not.toHaveBeenCalled()
    await userEvent.click(screen.getByRole('button', { name: 'Jump in' }))
    await done
    expect(teleport).toHaveBeenCalledWith(52, 2)
  })

  it('in a World, names that World’s place', async () => {
    const done = start(() => confirmTeleport(1, 1, 'boedo.dcl.eth', vi.fn()))
    await screen.findByText('Genesis Plaza')
    expect(fetchPlaceAt).toHaveBeenCalledWith({ x: 1, y: 1 }, 'boedo.dcl.eth')
    act(() => closeTopPopup())
    await done
  })

  it('without a named realm, shows the coordinates alone', async () => {
    const done = start(() => confirmTeleport(1, 1, null, vi.fn()))
    expect(await screen.findByText('1,1')).toBeTruthy()
    expect(fetchPlaceAt).not.toHaveBeenCalled()
    act(() => closeTopPopup())
    await done
  })

  it('Cancel and dismissal leave the player in place', async () => {
    const teleport = vi.fn()
    let done = start(() => confirmTeleport(52, 2, null, teleport))
    await userEvent.click(await screen.findByRole('button', { name: 'Cancel' }))
    await done
    done = start(() => confirmTeleport(52, 2, null, teleport))
    await screen.findByRole('button', { name: 'Jump in' })
    act(() => closeTopPopup()) // Escape and the scrim close the top popup
    await done
    expect(teleport).not.toHaveBeenCalled()
  })
})

describe('link confirmation', () => {
  it('shows the host and opens a new tab only after Continue', async () => {
    const open = vi.spyOn(window, 'open').mockReturnValue(null)
    const openExternal = vi.fn(async () => true)
    const done = start(() => confirmOpenLink('https://decentraland.org/play', openExternal))
    expect(await screen.findByText('decentraland.org')).toBeTruthy()
    expect(screen.getByText('https://decentraland.org/play')).toBeTruthy()
    expect(open).not.toHaveBeenCalled()
    await userEvent.click(screen.getByRole('button', { name: 'Continue' }))
    await done
    expect(open).toHaveBeenCalledWith('https://decentraland.org/play', '_blank', 'noopener,noreferrer')
    expect(openExternal).not.toHaveBeenCalled()
  })

  it('dismissal opens nothing', async () => {
    const open = vi.spyOn(window, 'open').mockReturnValue(null)
    const done = start(() => confirmOpenLink('https://decentraland.org', vi.fn(async () => true)))
    await screen.findByRole('button', { name: 'Continue' })
    act(() => closeTopPopup())
    await done
    expect(open).not.toHaveBeenCalled()
  })

  it('on the native HUD the engine opens it, and a refusal is shown', async () => {
    host.native = true
    const open = vi.spyOn(window, 'open').mockReturnValue(null)
    const openExternal = vi.fn(async () => false)
    const done = start(() => confirmOpenLink('http://example.com', openExternal))
    await userEvent.click(await screen.findByRole('button', { name: 'Continue' }))
    await done
    expect(openExternal).toHaveBeenCalledWith('http://example.com')
    expect(open).not.toHaveBeenCalled()
    expect(await screen.findByText("Couldn't open this link")).toBeTruthy()
  })
})
