// The Create page: a menu page whose body belongs to the scene editor, which renders its home
// (the creator's scenes) into it. Opening a scene closes the page and takes the screen.

import { useEffect, useRef } from 'react'
import { showToast, Spinner } from '../../design'
import { MainMenuShell } from '../menu/MainMenuShell'
import { EditorOffered } from './EditorOffered'
import type { EngineSession, ProfileState } from '../session/useEngineSession'
import type { EditorEntry } from './EditorHost'
import styles from './CreatePage.module.css'

function Home({ entry, close }: { entry: EditorEntry; close: () => void }): React.JSX.Element {
  const ref = useRef<HTMLDivElement>(null)
  const closeRef = useRef(close)
  closeRef.current = close
  const { load } = entry

  useEffect(() => {
    const el = ref.current!
    let gone = false
    let unmount: (() => void) | null = null
    load()
      .then((editor) => {
        if (!gone) unmount = editor.mountHome(el, { close: () => closeRef.current() })
      })
      .catch((e: unknown) => {
        if (gone) return
        console.error('[create] the editor home failed', e)
        showToast('Create could not be loaded. Try again in a moment.', { tone: 'error' })
        closeRef.current()
      })
    return () => {
      gone = true
      try {
        unmount?.()
      } catch (e) {
        console.error('[create] unmounting the editor home failed', e)
      }
    }
  }, [load])

  return (
    <div className={styles.body}>
      <div ref={ref} id="dcl-editor-home" className={styles.home} />
      {entry.loading && <Opening />}
    </div>
  )
}

function Opening(): React.JSX.Element {
  return (
    <div className={styles.opening} role="status">
      <Spinner size={20} />
      Opening Create…
    </div>
  )
}

/** What the Create page shows while the editor host is still loading. */
export function CreateLoading(): React.JSX.Element {
  return (
    <div className={styles.body}>
      <Opening />
    </div>
  )
}

/** The Create page's menu shell, shown while Create is open. */
export function CreateShell({
  create,
  profile,
  onNavigate,
  children
}: {
  create: EngineSession['create']
  profile: ProfileState
  onNavigate: (page: string) => void
  children: React.ReactNode
}): React.JSX.Element | null {
  if (!create.open) return null
  const p = profile.data
  return (
    // this page is outside the HUD's in-world tree, where App provides it
    <EditorOffered.Provider value>
      <MainMenuShell
        active="create"
        profileName={p?.name}
        profilePicture={p?.picture}
        profileAddress={p?.address}
        profileClaimed={p?.hasClaimedName}
        onNavigate={onNavigate}
        onClose={() => create.show(false)}
      >
        {children}
      </MainMenuShell>
    </EditorOffered.Provider>
  )
}

export function CreatePage({
  entry,
  create,
  profile,
  onNavigate
}: {
  entry: EditorEntry
  create: EngineSession['create']
  profile: ProfileState
  onNavigate: (page: string) => void
}): React.JSX.Element {
  return (
    <CreateShell create={create} profile={profile} onNavigate={onNavigate}>
      <Home entry={entry} close={() => create.show(false)} />
    </CreateShell>
  )
}
