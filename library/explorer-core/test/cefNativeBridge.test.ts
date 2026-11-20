import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { installCefNativeBridge } from '../lib/cefNativeBridge'

describe("the native bridge's clipboard", () => {
  // jsdom has no clipboard: each test defines the webview's, and it goes with the test
  const browserWrite = vi.fn(async (_text: string) => undefined)
  let emitted: unknown[]
  let hear: (id: string, payload: unknown) => void
  // the bridge listens on the document for the page's life: a test takes its listeners away
  let listening: [string, EventListenerOrEventListenerObject][]

  const installNative = (): void => {
    const listeners = new Map<string, (payload: unknown) => void>()
    hear = (id, payload) => listeners.get(id)?.(payload)
    ;(window as unknown as { cef: unknown }).cef = {
      emit: (payload: unknown) => emitted.push(payload),
      listen: (id: string, cb: (payload: unknown) => void) => listeners.set(id, cb)
    }
    vi.stubGlobal('BroadcastChannel', class { postMessage() {} })
    installCefNativeBridge()
  }
  /** The engine's window is on Wayland. */
  const installOnWayland = (): void => {
    installNative()
    hear('engineClipboard', '')
  }

  beforeEach(() => {
    emitted = []
    listening = []
    Object.defineProperty(navigator, 'clipboard', { value: { writeText: browserWrite }, configurable: true })
    const listen = document.addEventListener.bind(document)
    vi.spyOn(document, 'addEventListener').mockImplementation((type: string, listener: EventListenerOrEventListenerObject) => {
      listening.push([type, listener])
      listen(type, listener)
    })
  })
  afterEach(() => {
    for (const [type, listener] of listening) document.removeEventListener(type, listener)
    delete (window as unknown as { cef?: unknown }).cef
    Reflect.deleteProperty(navigator, 'clipboard')
    browserWrite.mockClear()
    document.body.innerHTML = ''
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  it('sends what the page copies to the engine: a written text, and a copied or cut selection', async () => {
    installOnWayland()

    await expect(navigator.clipboard.writeText('0xabc')).resolves.toBeUndefined()
    expect(browserWrite).not.toHaveBeenCalled()

    document.body.innerHTML = '<p>picked words</p>'
    document.dispatchEvent(new Event('copy'))
    document.getSelection()!.selectAllChildren(document.body.firstElementChild!)
    document.dispatchEvent(new Event('copy'))
    document.dispatchEvent(new Event('cut'))

    const copied = (text: string): object => ({ to: 'engine', kind: 'copyToClipboard', text })
    expect(emitted).toEqual([copied('0xabc'), copied('picked words'), copied('picked words')])
  })

  it('asks the engine for a paste instead of pasting, and types its answer', () => {
    installOnWayland()
    const execCommand = vi.fn(() => true)
    document.execCommand = execCommand

    expect(document.dispatchEvent(new Event('paste', { cancelable: true }))).toBe(false)
    expect(emitted).toEqual([{ to: 'engine', kind: 'paste' }])
    expect(execCommand).not.toHaveBeenCalled()

    hear('paste', 'from the session')
    expect(execCommand).toHaveBeenCalledWith('insertText', false, 'from the session')
  })

  it.each([
    ['until the engine asks for it (a window on X11, macOS, Windows)', installNative],
    ['where window.cef is absent (the web)', installCefNativeBridge]
  ])("keeps the webview's clipboard %s", async (_where, install) => {
    install()

    await navigator.clipboard.writeText('0xabc')
    expect(browserWrite).toHaveBeenCalledWith('0xabc')
    expect(document.dispatchEvent(new Event('paste', { cancelable: true }))).toBe(true)
    expect(listening).toEqual([])
    expect(emitted).toEqual([])
  })
})
