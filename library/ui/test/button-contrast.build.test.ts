import { afterEach, describe, it, expect } from 'vitest'
import { productionBrowser } from './production-browser.helpers'
import { colorContrast, requiredTextContrast } from './color-contrast.helpers'
const browser = productionBrowser('test/consumer/button-contrast.tsx')
type Metrics = {
  color: string
  background: string
  fontSize: string
  fontWeight: string
  opacity: string
  x: number
  y: number
  width: number
  height: number
  outline: string
  outlineColor: string
  outlineOffset: string
  shadow: string
  disabled: boolean
  busy: string | null
  focused: boolean
}
const sizes = ['xl', 'lg', 'md', 'sm'] as const
const contexts = sizes.flatMap((size) =>
  ['desktop', 'mobile'].map((viewport) => ({ size, viewport }))
)
async function ready(query: string) {
  await browser.session.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: 0, y: 0 })
  await browser.session.navigate(browser.origin + query)
  await browser.evaluate(
    `(async()=>{while(!document.querySelector('#target'))await new Promise(r=>requestAnimationFrame(r));await document.fonts.ready})()`
  )
}
async function metrics() {
  return browser.evaluate<Metrics>(
    `(()=>{const b=document.querySelector('#target'),s=getComputedStyle(b),r=b.getBoundingClientRect();return{color:s.color,background:s.backgroundColor,fontSize:s.fontSize,fontWeight:s.fontWeight,opacity:s.opacity,x:r.x,y:r.y,width:r.width,height:r.height,outline:s.outlineStyle,outlineColor:s.outlineColor,outlineOffset:s.outlineOffset,shadow:s.boxShadow,disabled:b.disabled,busy:b.getAttribute('aria-busy'),focused:document.activeElement===b}})()`
  )
}
async function key(key: string, shift = false) {
  for (const type of ['keyDown', 'keyUp'])
    await browser.session.send('Input.dispatchKeyEvent', {
      type,
      key,
      code: key,
      windowsVirtualKeyCode: key === 'Tab' ? 9 : key === 'Enter' ? 13 : 32,
      modifiers: shift ? 8 : 0,
      text:
        type === 'keyDown' ? (key === ' ' ? ' ' : key === 'Enter' ? '\r' : undefined) : undefined
    })
}
async function pointer(type: 'mouseMoved' | 'mousePressed' | 'mouseReleased', m: Metrics) {
  await browser.session.send('Input.dispatchMouseEvent', {
    type,
    x: m.x + m.width / 2,
    y: m.y + m.height / 2,
    ...(type === 'mouseMoved' ? {} : { button: 'left', clickCount: 1 })
  })
}
function contrast(m: Metrics) {
  return colorContrast(m.color, m.background)
}
afterEach(async () => {
  if (browser.session) {
    await browser.session.send('Emulation.setTouchEmulationEnabled', { enabled: false })
    await browser.session.send('Emulation.setEmulatedMedia', { features: [] })
  }
})
describe('when the shared primary button uses its actual font metrics', () => {
  it.each(contexts)(
    'should retain readable normal, hover and pressed text for $size $viewport',
    async (context) => {
      await ready('?size=' + context.size + '&viewport=' + context.viewport)
      const normal = await metrics()
      await pointer('mouseMoved', normal)
      const hover = await metrics()
      await pointer('mousePressed', normal)
      const pressed = await metrics()
      await pointer('mouseReleased', normal)
      for (const m of [normal, hover, pressed]) {
        expect(contrast(m)).toBeGreaterThanOrEqual(requiredTextContrast(m.fontSize, m.fontWeight))
        expect([m.width, m.height]).toEqual([normal.width, normal.height])
      }
      expect(await browser.evaluate(`document.querySelector('output').textContent`)).toBe('1')
    }
  )
  it.each(sizes)(
    'should preserve readable actual coarse input and a real native action for %s',
    async (size) => {
      await browser.session.send('Emulation.setTouchEmulationEnabled', {
        enabled: true,
        maxTouchPoints: 1
      })
      await ready('?size=' + size + '&viewport=mobile')
      const normal = await metrics()
      await browser.session.send('Input.dispatchTouchEvent', {
        type: 'touchStart',
        touchPoints: [{ x: normal.x + normal.width / 2, y: normal.y + normal.height / 2, id: 1 }]
      })
      const pressed = await metrics()
      await browser.session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
      for (const m of [normal, pressed])
        expect(contrast(m)).toBeGreaterThanOrEqual(requiredTextContrast(m.fontSize, m.fontWeight))
      expect(await browser.evaluate(`document.querySelector('output').textContent`)).toBe('1')
    }
  )
  it.each(['dark', 'light'])(
    'should retain real keyboard focus paint on the %s host without moving its target',
    async (theme) => {
      await ready('?theme=' + theme)
      const before = await metrics()
      await key('Tab')
      await key('Tab')
      const focused = await metrics()
      expect(focused.focused).toBe(true)
      expect([focused.width, focused.height]).toEqual([before.width, before.height])
      const png = (await browser.session.send('Page.captureScreenshot', { format: 'png' })) as {
        data: string
      }
      const pixels = await browser.evaluate<number[][]>(
        `(async()=>{const image=new Image();image.src='data:image/png;base64,${png.data}';await image.decode();const canvas=document.createElement('canvas');canvas.width=image.width;canvas.height=image.height;const ctx=canvas.getContext('2d');ctx.drawImage(image,0,0);return[${Math.floor(focused.x - 2)},${Math.floor(focused.x - 4)},${Math.floor(focused.x - 8)}].map(x=>Array.from(ctx.getImageData(x,${Math.floor(focused.y + focused.height / 2)},1,1).data))})()`
      )
      const rgb = (pixel: number[]) => 'rgb(' + pixel.slice(0, 3).join(',') + ')'
      expect(
        Math.max(
          colorContrast(rgb(pixels[0]!), rgb(pixels[2]!)),
          colorContrast(rgb(pixels[1]!), rgb(pixels[2]!))
        )
      ).toBeGreaterThanOrEqual(3)
      expect(contrast(focused)).toBeGreaterThanOrEqual(
        requiredTextContrast(focused.fontSize, focused.fontWeight)
      )
      await key('Enter')
      expect(await browser.evaluate(`document.querySelector('output').textContent`)).toBe('1')
    }
  )
  it.each(sizes)(
    'should remain readable in forced colors without changing %s target geometry',
    async (size) => {
      await ready('?size=' + size)
      const normal = await metrics()
      await browser.session.send('Emulation.setEmulatedMedia', {
        features: [{ name: 'forced-colors', value: 'active' }]
      })
      await ready('?size=' + size)
      const forced = await metrics()
      expect([forced.width, forced.height]).toEqual([normal.width, normal.height])
      expect(contrast(forced)).toBeGreaterThanOrEqual(
        requiredTextContrast(forced.fontSize, forced.fontWeight)
      )
      await key('Tab')
      await key('Tab')
      expect((await metrics()).focused).toBe(true)
    }
  )
  it.each(['disabled', 'loading'])(
    'should preserve inactive native %s state and suppress actual pointer activation',
    async (state) => {
      await ready('?' + state)
      const m = await metrics()
      await pointer('mouseMoved', m)
      await pointer('mousePressed', m)
      await pointer('mouseReleased', m)
      expect((await metrics()).disabled).toBe(state === 'disabled')
      expect(await browser.evaluate(`document.querySelector('output').textContent`)).toBe('0')
      expect((await metrics()).color).toBe('rgb(255, 255, 255)')
      if (state === 'loading') expect((await metrics()).busy).toBe('true')
    }
  )
})

describe('when the primary paint correction shares the native Button root', () => {
  it.each([
    ['secondary', 'rgb(252, 252, 252)'],
    ['dark', 'rgb(255, 255, 255)'],
    ['outline', 'rgb(252, 252, 252)'],
    ['outlineOnLight', 'rgb(22, 21, 24)'],
    ['inverse', 'rgb(37, 33, 42)'],
    ['text', 'rgb(252, 252, 252)']
  ])('should preserve the existing %s foreground', async (variant, color) => {
    await ready('?variant=' + variant)
    expect((await metrics()).color).toBe(color)
  })
  it('should preserve the transparent icon-only foreground instead of applying filled-primary text paint', async () => {
    await ready('?compact')
    const m = await metrics()
    expect(m.color).toBe('rgb(255, 255, 255)')
    expect(m.background).toBe('rgba(0, 0, 0, 0)')
  })
  it.each(['hover', 'pressed', 'focus'])(
    'should also retain readable authored %s state previews',
    async (force) => {
      await ready('?force=' + force)
      const m = await metrics()
      expect(contrast(m)).toBeGreaterThanOrEqual(requiredTextContrast(m.fontSize, m.fontWeight))
    }
  )
})
