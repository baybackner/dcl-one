// Confirmations before a chat link leaves the HUD or a chat coordinate moves the player.

import { useEffect, useState } from 'react'
import { DEFAULT_REALM, isNativeHud } from '@hud-host'
import { fetchPlaceAt, isEmptyPlace } from '@explorer-core/features/map/mapApi'
import { showConfirm, showToast } from '../../design'
import styles from './chatLinkPrompts.module.css'

/** Asks before opening `url`, naming its host first. The native HUD has no tabs, so the engine opens it there. */
export async function confirmOpenLink(url: string, openExternal: (url: string) => Promise<boolean>): Promise<void> {
  const ok = await showConfirm({ title: 'Open this link?', body: <LinkTarget url={url} />, confirmLabel: 'Continue' })
  if (!ok) return
  if (!isNativeHud()) {
    window.open(url, '_blank', 'noopener,noreferrer')
    return
  }
  const opened = await openExternal(url).catch(() => false)
  if (!opened) showToast("Couldn't open this link", { tone: 'error' })
}

/** Asks before teleporting to parcel `x,y` of the current realm (`realm`, null when it has no name). */
export async function confirmTeleport(x: number, y: number, realm: string | null, teleport: (x: number, y: number) => void): Promise<void> {
  const ok = await showConfirm({ title: 'Jump to this location?', body: <TeleportTarget x={x} y={y} realm={realm} />, confirmLabel: 'Jump in' })
  if (ok) teleport(x, y)
}

/** The host on its own line, then the whole URL kept left-to-right, so bidi controls cannot reorder it. */
function LinkTarget({ url }: { url: string }): React.JSX.Element {
  let host: string | null = null
  try {
    host = new URL(url).hostname
  } catch {
    host = null
  }
  return (
    <>
      {host != null && <p className={styles.host}>{host}</p>}
      <p className={styles.url} dir="ltr">
        <bdi>{url}</bdi>
      </p>
    </>
  )
}

/** The parcel, and the place covering it in the current realm once the places API answers. */
function TeleportTarget({ x, y, realm }: { x: number; y: number; realm: string | null }): React.JSX.Element {
  const [title, setTitle] = useState<string | null>(null)
  useEffect(() => {
    if (realm == null) return
    let live = true
    fetchPlaceAt({ x, y }, realm === DEFAULT_REALM ? undefined : realm)
      .then((place) => {
        if (live && place != null && !isEmptyPlace(place) && place.title !== '') setTitle(place.title)
      })
      .catch(() => undefined)
    return () => {
      live = false
    }
  }, [x, y, realm])
  return (
    <p>
      {title != null && <strong>{title} </strong>}
      {x},{y}
    </p>
  )
}
