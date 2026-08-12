import { describe, expect, it } from 'vitest'

import tauriConf from '../../../../src-tauri/tauri.conf.json'

import {
  PARTIAL_HEIGHT,
  PARTIAL_LINE_HEIGHT,
  PARTIAL_LINES,
  WINDOW_HEIGHT,
  pillHeight,
} from '../layout'

// The HUD's window is fixed-size, undecorated and transparent, so a pill too
// tall for it has no way to say so: it lays out past an edge, and what is
// past the edge is clipped rather than scrolled to. Nothing the frontend can
// assert at runtime catches that — by then the DOM is perfectly correct and
// the pixels are gone — so the arithmetic is asserted here instead, against
// the window `tauri.conf.json` asks for. That request is a floor, not the
// measurement (see `WINDOW_HEIGHT`), which is exactly what makes it worth
// asserting: it is the smallest window the pill may ever be given.
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
