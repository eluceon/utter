// The HUD pill's vertical geometry, in CSS pixels, in one place.
//
// This lives outside `Hud.svelte` because the numbers have to agree with
// something the stylesheet cannot see: the `hud` window in
// `apps/desktop/src-tauri/tauri.conf.json`. That window is fixed-size,
// undecorated and transparent, so a pill too tall for it has nowhere to go —
// no scrollbar, no resize, just rows laid out past an edge the compositor
// clips at. `__tests__/layout.test.ts` pins the two together.
//
// `Hud.svelte` feeds every one of these to its stylesheet as a custom
// property (see `HUD_STYLE`), so the arithmetic here is the arithmetic the
// browser performs.

/**
 * The height the `hud` window is *requested* at in `tauri.conf.json`.
 *
 * A request, not a measurement: GTK/WebKitGTK will not give a toplevel
 * webview window an inner height below 200px, so on Linux the real window is
 * always taller than this. The number still has to be right — it is the
 * lower bound the pill is guaranteed, and the only one under our control.
 */
export const WINDOW_HEIGHT = 104

/** Padding inside the pill, above the first row and below the last. */
export const PILL_PADDING_Y = 10

/** Vertical space between the pill's rows. */
export const ROW_GAP = 8

/** The phase row: status dot plus phase label. */
export const STATUS_ROW_HEIGHT = 16

/** The input-level meter. */
export const METER_HEIGHT = 20

/** One line box of live-preview text. */
export const PARTIAL_LINE_HEIGHT = 15

/**
 * How many lines of live preview the pill shows. The preview grows word by
 * word, so this is a *fixed* reservation rather than a maximum: the pill is
 * the same height on the first word as on the fiftieth, and the text scrolls
 * inside it (newest lines pinned to the bottom). A pill that grew with the
 * sentence would resize an always-on-top window at the rate speech is
 * recognized, over whatever the user is actually working in.
 */
export const PARTIAL_LINES = 2

/** The fixed height the preview row occupies when it is shown at all. */
export const PARTIAL_HEIGHT = PARTIAL_LINE_HEIGHT * PARTIAL_LINES

/**
 * The pill's rendered height, with and without the live-preview row.
 *
 * The preview row is not reserved when there is no preview to show: the
 * feature is off by default and off entirely for every engine that cannot
 * stream, and those users get the same 64px pill they had before it existed.
 * The *window* is sized for the taller of the two states, which costs
 * nothing visually — it is transparent, so the unused strip below the pill
 * is not drawn.
 */
export function pillHeight(showsPartial: boolean): number {
  const base = PILL_PADDING_Y * 2 + STATUS_ROW_HEIGHT + ROW_GAP + METER_HEIGHT
  return showsPartial ? base + ROW_GAP + PARTIAL_HEIGHT : base
}

/** The constants above, as the inline custom properties `Hud.svelte` sets. */
export const HUD_STYLE = [
  `--hud-pad-y: ${PILL_PADDING_Y}px`,
  `--hud-row-gap: ${ROW_GAP}px`,
  `--hud-status-row: ${STATUS_ROW_HEIGHT}px`,
  `--hud-meter: ${METER_HEIGHT}px`,
  `--hud-partial-line: ${PARTIAL_LINE_HEIGHT}px`,
  `--hud-partial: ${PARTIAL_HEIGHT}px`,
].join('; ')
