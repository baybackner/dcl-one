import { act, fireEvent, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import CommunityCreate from './CommunityCreate'

const POLICY = 'https://example.org/content'

function setup(create = vi.fn().mockResolvedValue({ id: 'c1' }), onDone = vi.fn()) {
  render(<CommunityCreate create={create} onDone={onDone} contentPolicyUrl={POLICY} />)
  return { create, onDone }
}

const nameField = () => screen.getByRole('textbox', { name: /Community name/i })
const createButton = () => screen.getByRole('button', { name: 'Create' })

describe('CommunityCreate', () => {
  afterEach(() => {
    vi.resetAllMocks()
  })

  describe('when the form opens', () => {
    it('should name the dialog and link the content policy', () => {
      setup()
      expect(screen.getByRole('dialog', { name: 'Create a Community' })).toBeInTheDocument()
      expect(screen.getByRole('link', { name: 'Content Policy' })).toHaveAttribute('href', POLICY)
      expect(createButton()).toHaveAttribute('aria-disabled', 'true')
    })
  })

  describe('when Create is pressed without a name', () => {
    it('should not send anything', async () => {
      const { create } = setup()
      await userEvent.type(nameField(), '   ')
      await userEvent.click(createButton())
      expect(create).not.toHaveBeenCalled()
    })
  })

  describe('when a named community is created with the defaults', () => {
    it('should send a public, listed community described by its name and report it created', async () => {
      const { create, onDone } = setup()
      await userEvent.type(nameField(), '  Builders  ')
      await userEvent.click(createButton())
      expect(create).toHaveBeenCalledWith({
        name: 'Builders',
        description: 'Builders',
        privacy: 'public',
        visibility: 'all'
      })
      expect(onDone).toHaveBeenCalledWith(true)
    })
  })

  describe('when the membership is private and the community is hidden', () => {
    it('should send a private, unlisted community with its description', async () => {
      const { create } = setup()
      await userEvent.type(nameField(), 'Builders')
      await userEvent.type(screen.getByRole('textbox', { name: 'Description' }), 'We build')
      await userEvent.click(screen.getByRole('combobox', { name: 'Membership' }))
      await userEvent.click(screen.getByRole('option', { name: /^Private/ }))
      await userEvent.click(
        screen.getByRole('switch', { name: 'Show the community in the directory' })
      )
      await userEvent.click(createButton())
      expect(create).toHaveBeenCalledWith({
        name: 'Builders',
        description: 'We build',
        privacy: 'private',
        visibility: 'unlisted'
      })
    })
  })

  describe('when the account has no NAME', () => {
    it('should show the NAME gate and return to the form from it', async () => {
      const { onDone } = setup(
        vi.fn().mockRejectedValue({ status: 401, message: "The user doesn't have any names" })
      )
      await userEvent.type(nameField(), 'Builders')
      await userEvent.click(createButton())
      expect(
        screen.getByRole('dialog', { name: 'Get a NAME to Unlock Community Creation' })
      ).toBeInTheDocument()
      expect(onDone).not.toHaveBeenCalled()
      await userEvent.click(screen.getByRole('button', { name: 'Maybe later' }))
      expect(screen.getByRole('dialog', { name: 'Create a Community' })).toBeInTheDocument()
    })
  })

  describe('when the server refuses for another reason', () => {
    it('should announce its message and keep the form open', async () => {
      const { onDone } = setup(
        vi.fn().mockRejectedValue({ status: 401, message: 'Signature expired' })
      )
      await userEvent.type(nameField(), 'Builders')
      await userEvent.click(createButton())
      expect(screen.getByRole('alert')).toHaveTextContent('Signature expired')
      expect(onDone).not.toHaveBeenCalled()
    })
  })

  describe('when the failure has no message', () => {
    it('should announce a generic failure', async () => {
      setup(vi.fn().mockRejectedValue(undefined))
      await userEvent.type(nameField(), 'Builders')
      await userEvent.click(createButton())
      expect(screen.getByRole('alert')).toHaveTextContent("Couldn't create the community.")
    })
  })

  describe('while the community is being created', () => {
    it('should disable Cancel, refuse Escape and not send twice', async () => {
      let resolve: (value: unknown) => void = () => {}
      const create = vi.fn(() => new Promise((r) => (resolve = r)))
      const { onDone } = setup(create)
      await userEvent.type(nameField(), 'Builders')
      await userEvent.click(createButton())
      expect(screen.getByRole('button', { name: 'Creating…' })).toHaveAttribute(
        'aria-disabled',
        'true'
      )
      expect(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled()
      fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' })
      await userEvent.click(screen.getByRole('button', { name: 'Creating…' }))
      expect(create).toHaveBeenCalledTimes(1)
      expect(onDone).not.toHaveBeenCalled()
      await act(async () => resolve({ id: 'c1' }))
      expect(onDone).toHaveBeenCalledWith(true)
    })
  })

  describe('when the reader cancels', () => {
    it('should report nothing was created', async () => {
      const { create, onDone } = setup()
      await userEvent.click(screen.getByRole('button', { name: 'Cancel' }))
      expect(onDone).toHaveBeenCalledWith(false)
      expect(create).not.toHaveBeenCalled()
    })
  })
})
