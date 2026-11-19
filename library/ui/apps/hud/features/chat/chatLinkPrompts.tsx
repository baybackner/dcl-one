// Confirmations before a chat link leaves the HUD or a chat coordinate moves the player.

import { useEffect, useState } from 'react'
import { isNativeHud } from '@hud-host'
import { fetchPlaceAt } from '@explorer-core/features/lobby/lobbyApi'
import { showDialog } from '../../design'
import styles from './chatLinkPrompts.module.css'

/** Asks before opening `url`; opens it on CONTINUE. The native HUD has no tabs, so the engine opens it there. */
export async function confirmOpenLink(url: string, openExternal: (url: string) => Promise<boolean>): Promise<void> {
  const choice = await showDialog({
    title: 'Open this link?',
    body: <p className={styles.url}>{url}</p>,
    actions: [
      { id: 'cancel', label: 'Cancel', variant: 'secondary' },
      { id: 'open', label: 'Continue' }
    ]
  })
  if (choice !== 'open') return
  if (isNativeHud()) void openExternal(url)
  else window.open(url, '_blank', 'noopener,noreferrer')
}

/** Asks before teleporting to parcel `x,y`; calls `teleport` on JUMP IN. */
export async function confirmTeleport(x: number, y: number, teleport: (x: number, y: number) => void): Promise<void> {
  const choice = await showDialog({
    title: 'Jump to this location?',
    body: <TeleportTarget x={x} y={y} />,
    actions: [
      { id: 'cancel', label: 'Cancel', variant: 'secondary' },
      { id: 'jump', label: 'Jump in' }
    ]
  })
  if (choice === 'jump') teleport(x, y)
}

/** The parcel, and the place covering it once the places API answers. */
function TeleportTarget({ x, y }: { x: number; y: number }): React.JSX.Element {
  const [title, setTitle] = useState<string | null>(null)
  useEffect(() => {
    let live = true
    fetchPlaceAt(x, y)
      .then((place) => {
        if (live && place?.title) setTitle(place.title)
      })
      .catch(() => undefined)
    return () => {
      live = false
    }
  }, [x, y])
  return (
    <p>
      {title != null && <strong>{title} </strong>}
      {x},{y}
    </p>
  )
}
