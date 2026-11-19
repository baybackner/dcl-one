import { describe, it, expect, vi, afterEach } from 'vitest'
import { render, screen, act } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { PopupHost, resetPopups } from '../../design'
import { confirmOpenLink, confirmTeleport } from '../../features/chat/chatLinkPrompts'

vi.mock('@explorer-core/features/lobby/lobbyApi', () => ({
  fetchPlaceAt: vi.fn(async () => ({ title: 'Genesis Plaza' }))
}))

afterEach(() => {
  resetPopups()
  vi.restoreAllMocks()
})

describe('chat link confirmations', () => {
  it('teleports only after Jump in, naming the place', async () => {
    render(<PopupHost />)
    const teleport = vi.fn()
    let done!: Promise<void>
    act(() => {
      done = confirmTeleport(52, 2, teleport)
    })
    expect(await screen.findByText('Genesis Plaza')).toBeTruthy()
    expect(teleport).not.toHaveBeenCalled()
    await userEvent.click(screen.getByRole('button', { name: 'Jump in' }))
    await done
    expect(teleport).toHaveBeenCalledWith(52, 2)
  })

  it('Cancel leaves the player in place', async () => {
    render(<PopupHost />)
    const teleport = vi.fn()
    let done!: Promise<void>
    act(() => {
      done = confirmTeleport(52, 2, teleport)
    })
    await userEvent.click(await screen.findByRole('button', { name: 'Cancel' }))
    await done
    expect(teleport).not.toHaveBeenCalled()
  })

  it('opens the link in a new tab only after Continue', async () => {
    render(<PopupHost />)
    const open = vi.spyOn(window, 'open').mockReturnValue(null)
    const openExternal = vi.fn(async () => true)
    let done!: Promise<void>
    act(() => {
      done = confirmOpenLink('https://decentraland.org', openExternal)
    })
    expect(await screen.findByText('https://decentraland.org')).toBeTruthy()
    expect(open).not.toHaveBeenCalled()
    await userEvent.click(screen.getByRole('button', { name: 'Continue' }))
    await done
    expect(open).toHaveBeenCalledWith('https://decentraland.org', '_blank', 'noopener,noreferrer')
    expect(openExternal).not.toHaveBeenCalled()
  })
})
