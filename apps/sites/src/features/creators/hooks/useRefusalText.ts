import { useTranslation } from '@/data/lib/creators/intl'
import type { BoundRefusal } from '@/data/lib/creators/builder'

const MINUTE_SECS = 60

/** What the creator reads for the node's refusal at one of its bounds: its words, then when to try again if it said. */
export function useRefusalText(): (refusal: BoundRefusal | null) => string | null {
  const { t } = useTranslation()
  return refusal => {
    if (!refusal) return null
    const { message: reason, retryAfterSecs } = refusal
    if (retryAfterSecs === undefined) return reason
    return retryAfterSecs < MINUTE_SECS
      ? t('builder_refusal.retry_in_seconds', { reason, count: retryAfterSecs })
      : t('builder_refusal.retry_in_minutes', { reason, count: Math.ceil(retryAfterSecs / MINUTE_SECS) })
  }
}
