import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { EventDetailModal } from './DetailModal'

afterEach(cleanup)

describe('when an upcoming event offers a reminder', () => {
  let showReminder: (active: boolean) => React.JSX.Element

  beforeEach(() => {
    const onToggle = vi.fn()
    const onClose = vi.fn()
    showReminder = (active) => (
      <EventDetailModal
        title="Open studio"
        image={null}
        live={false}
        location={{ label: '0,0', world: false }}
        remind={{ active, pending: false, onToggle }}
        onClose={onClose}
      />
    )
  })

  it('should show the secondary-light medium variant before the reminder is set', () => {
    render(showReminder(false))
    const button = screen.getByRole('button', { name: 'Remind me', pressed: false })
    expect(button.getAttribute('data-variant')).toBe('secondaryLight')
    expect(button.getAttribute('data-size')).toBe('md')
  })

  it('should keep focus on the same button as the reminder becomes active', () => {
    const { rerender } = render(showReminder(false))
    const button = screen.getByRole('button', { name: 'Remind me', pressed: false })
    button.focus()

    rerender(showReminder(true))

    expect(screen.getByRole('button', { name: 'Remind me', pressed: true })).toBe(button)
    expect(document.activeElement).toBe(button)
  })
})
