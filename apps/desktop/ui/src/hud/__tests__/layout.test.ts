import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

import { describe, expect, it } from 'vitest'

import tauriConf from '../../../../src-tauri/tauri.conf.json'

import {
  HUD_STYLE,
  PARTIAL_HEIGHT,
  PARTIAL_LINE_HEIGHT,
  PARTIAL_LINES,
  WINDOW_HEIGHT,
  pillHeight,
} from '../layout'

// The HUD's window is fixed-size, undecorated and transparent, so a pill too
// tall for it has no way to say so: it lays out past an edge, and what is
// past the edge is clipped rather than scrolled to. That is what hid the live
// preview. Nothing the frontend can assert at runtime catches it — by then
// the DOM is perfectly correct and the pixels are gone — so the arithmetic is
// asserted here instead, against the window `tauri.conf.json` asks for and is
// given (see `WINDOW_HEIGHT`).
const hudWindow = tauriConf.app.windows.find((w) => w.label === 'hud')!

/** The pill's height before the live preview existed; see below. */
const PILL_HEIGHT_WITHOUT_PREVIEW = 64

describe('hud layout', () => {
  it('declares the same window height the app requests for the hud', () => {
    expect(hudWindow.height).toBe(WINDOW_HEIGHT)
  })

  it('fits the pill inside the requested window with the live preview showing', () => {
    expect(pillHeight(true)).toBeLessThanOrEqual(WINDOW_HEIGHT)
  })

  it('fits the pill inside the requested window with no live preview', () => {
    expect(pillHeight(false)).toBeLessThanOrEqual(WINDOW_HEIGHT)
  })

  // The live preview is off by default and unavailable to engines that
  // cannot stream. Those users must keep exactly the pill they had, rather
  // than a taller one with a permanently empty strip in it.
  it('leaves the pill at its pre-preview height when there is no preview', () => {
    expect(pillHeight(false)).toBe(PILL_HEIGHT_WITHOUT_PREVIEW)
  })

  // The preview row is bottom-anchored and clipped at the top, so it only
  // ever hides *whole* lines; a height that isn't a multiple of the line box
  // would leave a sliver of the line above showing above the newest text.
  it('reserves a whole number of preview lines', () => {
    expect(PARTIAL_HEIGHT).toBe(PARTIAL_LINE_HEIGHT * PARTIAL_LINES)
    expect(PARTIAL_HEIGHT % PARTIAL_LINE_HEIGHT).toBe(0)
  })
})

// Everything above asserts the *model* — numbers agreeing with other numbers.
// The defect was in the gap between the model and the rendered box, and these
// three assertions guard the parts of that gap that are legible in source:
// that the numbers are handed to the stylesheet at all, that the stylesheet
// reads exactly the ones it is handed, and that nothing pins the pill's
// height behind their backs.
//
// They are a source check, not a measurement. What they cannot do is lay the
// pill out: none of them would notice a row that overflows for some reason
// nobody wrote down here, and jsdom would not either — it has no layout
// engine, so a rendered-DOM test of this would report every box as zero and
// pass no matter what. Only a real engine measures: driving the built HUD in
// headless Chromium and reading `.hud`'s `getBoundingClientRect().height`
// against `pillHeight()` in both states is the test that would, and it needs
// browser test infrastructure this repo does not have.
const HUD_SVELTE = readFileSync(
  fileURLToPath(new URL('../Hud.svelte', import.meta.url)),
  'utf8',
)

/** The `--hud-*` names `HUD_STYLE` defines. */
const declared = HUD_STYLE.split(';')
  .map((decl) => decl.trim().split(':')[0])
  .filter((name) => name.startsWith('--hud-'))

describe('hud stylesheet binding', () => {
  // Without this attribute every `--hud-*` below is unset and every row falls
  // back to its intrinsic height: `layout.ts` becomes arithmetic about
  // nothing, and all of the assertions above stay green while it does.
  it('hands the layout constants to the pill as inline custom properties', () => {
    expect(HUD_SVELTE).toContain('style={HUD_STYLE}')
  })

  // Both directions. A property defined and never read is dead arithmetic; a
  // property read and never defined resolves to nothing, which is how a row
  // silently collapses to zero height — the preview row's exact failure.
  it('defines exactly the custom properties the stylesheet reads', () => {
    const read = [...HUD_SVELTE.matchAll(/var\((--hud-[a-z-]+)\)/g)].map((m) => m[1])

    expect(declared.length).toBeGreaterThan(0)
    expect(read.length).toBeGreaterThan(0)
    expect([...new Set(read)].sort()).toEqual([...new Set(declared)].sort())
  })

  // The pill must size itself from its rows. A fixed height does not overflow
  // when a row does not fit — it shrinks the rows instead, which is how the
  // preview row was squeezed to a few pixels and then clipped away by its own
  // `overflow: hidden`, present in the DOM and never once drawn.
  it('pins no height on the pill itself', () => {
    const rule = /\.hud\s*\{([^}]*)\}/.exec(HUD_SVELTE)
    expect(rule, 'the .hud rule must exist to be checked').not.toBeNull()
    expect(rule![1]).not.toMatch(/(^|[\s;])(min-|max-)?height\s*:/)
  })
})
