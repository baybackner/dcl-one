import { renderToStaticMarkup } from 'react-dom/server'
import { describe, expect, it } from 'vitest'
import { readCopy } from '../../../../web-core/freshness/copyAsOf'
import FreshnessNotice from './FreshnessNotice'

const NOW = Date.parse('2026-10-02T12:00:00Z')
const HOUR = 3_600_000

const stamp = (ageMs: number, staleAfterSecs?: number) => ({
  as_of: new Date(NOW - ageMs).toISOString(),
  ...(staleAfterSecs === undefined ? {} : { stale_after_secs: staleAfterSecs }),
  data: []
})

describe('FreshnessNotice', () => {
  it('says how old a mirrored copy is once it is past the budget its node sent, and nothing before', () => {
    const html = (body: unknown) =>
      renderToStaticMarkup(<FreshnessNotice copy={readCopy(body)} now={NOW} />)
    expect(html(stamp(3 * HOUR, 7200))).toContain('as of 3 hours ago')
    expect(html(stamp(26 * HOUR, 7200))).toContain('as of 1 day ago')
    expect(html(stamp(HOUR, 7200))).toBe('')
    expect(
      html(stamp(3 * HOUR, 4 * 3600)),
      'a node with a longer interval sends a longer budget'
    ).toBe('')
    expect(html(stamp(10 * 60_000, 300))).toContain('as of 10 minutes ago')
    expect(html(stamp(HOUR)), 'no budget: said at once').toContain('as of 1 hour ago')
    expect(html({ data: [] })).toBe('')
    const failed = renderToStaticMarkup(<FreshnessNotice failed onRetry={() => {}} now={NOW} />)
    expect(failed).toContain('Showing saved results')
    expect(failed).not.toContain('copy-as-of')
  })
})
