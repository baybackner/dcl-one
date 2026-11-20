import { cleanup, render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { AllEvents, type AllEventsProps } from './AllEvents'
import type { MyEventCardProps } from './types'

afterEach(cleanup)

const DAY = { key: '2026-11-20', label: 'Today', fullLabel: 'Friday, November 20', isToday: true }
const HREFS = { all: '/events', my: '/events?tab=my' }

function card(over: Partial<MyEventCardProps> = {}): MyEventCardProps {
  return {
    id: 'e1',
    title: 'Poker Night',
    image: 'https://example.test/cover.png',
    href: '/events/edit-event/e1',
    time: { label: 'Tomorrow 8:00 PM', dateTime: '2026-11-21T20:00:00Z' },
    status: 'approved',
    ...over
  }
}

function mount(over: Partial<AllEventsProps> = {}) {
  return render(
    <AllEvents
      id="all-events"
      days={[{ ...DAY, cards: [] }]}
      layout="columns"
      canGoBack={false}
      onPrevious={() => {}}
      onNext={() => {}}
      {...over}
    />
  )
}

describe('AllEvents', () => {
  describe('when the visitor is signed out', () => {
    it('should show the section title and no views', () => {
      mount()
      expect(screen.getByRole('heading', { name: 'All Events' })).toBeTruthy()
      expect(screen.queryByRole('tablist')).toBeNull()
    })
  })

  describe('when a member is on All Events', () => {
    it('should mark All Events as the selected pill tab and show the days', () => {
      mount({ views: { active: 'all', hrefs: HREFS } })
      const tabs = screen.getByRole('tablist', { name: 'Events views' })
      expect(
        within(tabs).getByRole('tab', { name: 'All Events' }).getAttribute('aria-selected')
      ).toBe('true')
      expect(within(tabs).getByRole('tab', { name: 'My Events' }).getAttribute('href')).toBe(
        '/events?tab=my'
      )
      expect(screen.getByRole('tabpanel', { name: 'All Events' })).toBeTruthy()
      expect(screen.getByText('No events this day.')).toBeTruthy()
    })

    it('should hand a tab click to the page', async () => {
      const onFollow = vi.fn((_view: string, event: { preventDefault: () => void }) =>
        event.preventDefault()
      )
      mount({ views: { active: 'all', hrefs: HREFS, onFollow } })
      await userEvent.click(screen.getByRole('tab', { name: 'My Events' }))
      expect(onFollow).toHaveBeenCalledWith('my', expect.anything())
    })
  })

  describe('when a member is on My Events', () => {
    const views = { active: 'my' as const, hrefs: HREFS }

    it('should show the empty card with a Create event link when the member has no upcoming events', () => {
      mount({ views, mine: { cards: [], status: 'ready', createHref: '/events/new-event' } })
      const panel = screen.getByRole('tabpanel', { name: 'My Events' })
      expect(within(panel).getByText("You don't have any event created")).toBeTruthy()
      expect(within(panel).getByRole('link', { name: 'Create event' }).getAttribute('href')).toBe(
        '/events/new-event'
      )
      expect(screen.queryByRole('navigation', { name: 'Event days' })).toBeNull()
      expect(screen.getByRole('tab', { name: 'My Events' }).getAttribute('aria-selected')).toBe(
        'true'
      )
    })

    it('should list the events under Hosted by me, each linking to its editor with its review status', () => {
      mount({
        views,
        mine: {
          cards: [
            card(),
            card({
              id: 'e2',
              title: 'Draft Party',
              href: '/events/edit-event/e2',
              status: 'pending'
            }),
            card({
              id: 'e3',
              title: 'Nope',
              image: null,
              href: '/events/edit-event/e3',
              status: 'rejected'
            })
          ],
          status: 'ready',
          createHref: '/events/new-event'
        }
      })
      const panel = screen.getByRole('tabpanel', { name: 'My Events' })
      expect(within(panel).getByRole('heading', { name: 'Hosted by me' })).toBeTruthy()
      const items = within(panel).getAllByRole('listitem')
      expect(
        items.map((item) =>
          within(item)
            .getByRole('link', { name: /Poker Night|Draft Party|Nope/ })
            .getAttribute('href')
        )
      ).toEqual(['/events/edit-event/e1', '/events/edit-event/e2', '/events/edit-event/e3'])
      expect(within(items[0]!).getByText('Approved')).toBeTruthy()
      expect(within(items[1]!).getByText('Pending')).toBeTruthy()
      expect(within(items[2]!).getByText('Rejected')).toBeTruthy()
      expect(within(panel).queryByText("You don't have any event created")).toBeNull()
    })

    it('should say the events are loading, not that there are none', () => {
      mount({ views, mine: { cards: [], status: 'loading', createHref: '/events/new-event' } })
      expect(screen.getByRole('status').textContent).toBe('Loading your events…')
      expect(screen.queryByText("You don't have any event created")).toBeNull()
    })

    it('should report a failed read with a Retry, not an empty list', async () => {
      const onRetry = vi.fn()
      mount({
        views,
        mine: { cards: [], status: 'unavailable', createHref: '/events/new-event', onRetry }
      })
      const alert = screen.getByRole('alert')
      expect(alert.textContent).toContain("Couldn't load your events.")
      await userEvent.click(within(alert).getByRole('button', { name: 'Retry' }))
      expect(onRetry).toHaveBeenCalledOnce()
      expect(screen.queryByText("You don't have any event created")).toBeNull()
    })
  })
})
