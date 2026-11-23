import { useState } from 'react'

import { Button } from '../../../components/Button/Button'
import { Dropdown } from '../../../components/Dropdown/Dropdown'
import { Modal } from '../../../components/Modal/Modal'
import { Switch } from '../../../components/Switch/Switch'
import styles from './CommunityCreate.module.css'

type Membership = 'public' | 'private'

const MEMBERSHIP_OPTIONS: ReadonlyArray<{ value: Membership; label: string }> = [
  {
    value: 'public',
    label: 'Public  Anyone can become a member, view Community details, and join your Voice Streams'
  },
  {
    value: 'private',
    label: 'Private  Members must be approved by an owner or moderator before they can join'
  }
]

export type CommunityCreateInput = {
  name: string
  description: string
  privacy: Membership
  visibility: 'all' | 'unlisted'
}

/** The failure a `create` call rejects with: the server's message and, for an HTTP error, its status. */
type CreateFailure = { status?: number; message?: string }

export type CommunityCreateProps = {
  /** Sends the community to the server. A 401 whose message says the account has no NAME shows the NAME gate. */
  create: (input: CommunityCreateInput) => Promise<unknown>
  /** Called with `true` after a community is created, `false` when the reader cancels. */
  onDone?: (created: boolean) => void
  contentPolicyUrl: string
}

export default function CommunityCreate({
  create,
  onDone,
  contentPolicyUrl
}: CommunityCreateProps) {
  const [gated, setGated] = useState(false)
  const [name, setName] = useState('')
  const [desc, setDesc] = useState('')
  const [membership, setMembership] = useState<Membership>('public')
  const [discoverable, setDiscoverable] = useState(true)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const incomplete = !name.trim()

  const submit = async () => {
    if (incomplete || busy) return
    setBusy(true)
    setErr('')
    try {
      await create({
        name: name.trim(),
        description: desc.trim() || name.trim(),
        privacy: membership,
        visibility: discoverable ? 'all' : 'unlisted'
      })
      onDone?.(true)
    } catch (e) {
      const failure = (e ?? {}) as CreateFailure
      if (failure.status === 401 && /doesn't have any names/i.test(failure.message ?? '')) {
        setGated(true)
      } else {
        setErr(failure.message || "Couldn't create the community.")
      }
    } finally {
      setBusy(false)
    }
  }

  if (gated) {
    return (
      <Modal
        title="Get a NAME to Unlock Community Creation"
        onClose={() => setGated(false)}
        image={
          <svg
            className={styles.gateArt}
            viewBox="0 0 48 48"
            width="48"
            height="48"
            aria-hidden="true"
          >
            <path
              d="M14 20v-4a10 10 0 0 1 20 0v4M10 20h28v18H10z"
              fill="none"
              stroke="currentColor"
              strokeWidth="2.4"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        }
        description={
          <>
            NAMEs are unique Decentraland usernames that come with a{' '}
            <a
              className={styles.link}
              href="https://decentraland.org/blog/about-decentraland/decentraland-worlds-your-own-virtual-space"
              target="_blank"
              rel="noopener noreferrer"
            >
              World
            </a>
            , and unlock community creation.
          </>
        }
        actions={
          <>
            <Button variant="secondary" onClick={() => setGated(false)}>
              Maybe later
            </Button>
            <Button onClick={() => setGated(false)}>Get a NAME</Button>
          </>
        }
      />
    )
  }

  return (
    <Modal
      title="Create a Community"
      onClose={() => onDone?.(false)}
      dismissible={!busy}
      actions={
        <>
          <Button variant="secondary" onClick={() => onDone?.(false)} disabled={busy}>
            Cancel
          </Button>
          <Button aria-disabled={incomplete || busy} onClick={submit}>
            {busy ? 'Creating\u{2026}' : 'Create'}
          </Button>
        </>
      }
    >
      <div className={styles.body}>
        <div className={styles.group}>
          <div className={styles.label}>Profile picture</div>
          <div className={styles.hint}>PNG or JPG | 512x512 px | 500KB max</div>
          <div className={styles.picture}>
            <span className={styles.pictureImage} aria-hidden="true">
              <svg viewBox="0 0 24 24" width="34" height="34">
                <rect
                  x="3"
                  y="4"
                  width="18"
                  height="16"
                  rx="2"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.6"
                />
                <circle cx="8.5" cy="9.5" r="1.8" fill="currentColor" />
                <path
                  d="M5 18l4.5-5 3.5 4 2.5-2.5L21 17"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.6"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
            </span>
            <button type="button" className={styles.pictureEdit} aria-label="Edit profile picture">
              <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
                <path
                  d="M14.06 6.19l3.75 3.75M3 17.25V21h3.75L17.81 9.94a1.5 1.5 0 0 0 0-2.12l-1.63-1.63a1.5 1.5 0 0 0-2.12 0L3 17.25z"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.6"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
            </button>
          </div>
        </div>

        <div className={styles.group}>
          <label className={styles.label} htmlFor="ccr-name">
            Community name{' '}
            <span className={styles.required} aria-hidden="true">
              *
            </span>
          </label>
          <input
            id="ccr-name"
            className={styles.input}
            maxLength={30}
            placeholder="Write here"
            required
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
        </div>

        <div className={styles.group}>
          <label className={styles.label} htmlFor="ccr-desc">
            Description
          </label>
          <input
            id="ccr-desc"
            className={styles.input}
            maxLength={240}
            placeholder="What's your community about?"
            value={desc}
            onChange={(e) => setDesc(e.target.value)}
          />
        </div>

        <div className={styles.group}>
          <div className={styles.label} aria-hidden="true">
            Membership
          </div>
          <Dropdown
            label="Membership"
            labelHidden
            options={MEMBERSHIP_OPTIONS}
            value={membership}
            onChange={setMembership}
          />
        </div>

        <div className={styles.group}>
          <div className={styles.label} aria-hidden="true">
            Discoverability
          </div>
          <Switch
            label="Show the community in the directory"
            labelHidden
            checked={discoverable}
            onCheckedChange={setDiscoverable}
          />
        </div>

        {err && (
          <p className={styles.error} role="alert">
            {err}
          </p>
        )}

        <p className={styles.policy}>
          Please ensure Community content follows Decentraland&apos;s{' '}
          <a
            className={styles.link}
            href={contentPolicyUrl}
            target="_blank"
            rel="noopener noreferrer"
          >
            Content Policy
          </a>
          .
        </p>
      </div>
    </Modal>
  )
}
