import { staleCopyAge, type CopyStamp } from '../../../../web-core/freshness/copyAsOf'
import styles from './FreshnessNotice.module.css'

export default function FreshnessNotice({
  failed,
  onRetry,
  copy,
  now = Date.now()
}: {
  failed?: boolean
  onRetry?: () => void
  copy?: CopyStamp | null
  now?: number
}) {
  const age = staleCopyAge(copy, now)
  if (!failed && age === null) return null
  return (
    <div className={styles.root} role="status">
      {failed && <span>Showing saved results. We couldn&rsquo;t refresh them.</span>}
      {age !== null && (
        <span data-testid="copy-as-of">Showing this node&rsquo;s copy as of {age} ago.</span>
      )}
      {failed && onRetry && (
        <button className={styles.retry} type="button" onClick={onRetry}>
          Try again
        </button>
      )}
    </div>
  )
}
