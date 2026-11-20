import type { ReactNode } from 'react'
import styles from './Notice.module.css'

// TODO(figma-ds): the design system ships no alert; the Events pages share this one until it does.

type NoticeTone = 'error' | 'warning' | 'info' | 'success'

/**
 * An Events message in its own box: an error interrupts (role `alert`), the other tones wait their turn
 * (role `status`). Line breaks in a text child are kept, so a note can follow its lead on the next line.
 */
function Notice({ tone = 'info', children }: { tone?: NoticeTone; children: ReactNode }) {
  return (
    <p className={styles.notice} data-tone={tone} role={tone === 'error' ? 'alert' : 'status'}>
      {children}
    </p>
  )
}

export { Notice, type NoticeTone }
