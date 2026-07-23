// Pure hotkey-chord helpers shared by `components/HotkeyPicker.svelte`,
// extracted so the token-normalization logic is unit-testable without
// mounting a component. Mirrors the grammar
// `utter_inject::hotkey::parse_hotkey` accepts: `+`-separated tokens, each
// one of `ctrl`/`alt`/`shift`/`super` (the modifier names, canonicalized —
// the Rust parser also accepts `control` and `meta`/`win` as aliases for
// `ctrl`/`super`, but this picker only ever emits the canonical short forms)
// or a single letter/digit/`f1`..`f24` base key. A chord made entirely of
// modifiers (e.g. the default `ctrl+super`) is valid and accepted here too.

export const MODIFIER_ORDER = ['ctrl', 'alt', 'shift', 'super'] as const
export type ModifierToken = (typeof MODIFIER_ORDER)[number]

const MODIFIER_KEY_NAMES: Record<string, ModifierToken> = {
  Control: 'ctrl',
  Alt: 'alt',
  Shift: 'shift',
  Meta: 'super',
}

/** Derives the normalized hotkey token for a keyboard event, given both its
 * `code` (the physical key — layout- and modifier-independent) and `key`
 * (the character the layout actually produced).
 *
 * Letters and digits are read from `code` (`KeyA`..`KeyZ`, `Digit0`..`Digit9`)
 * rather than `key`: holding Shift changes `key` (e.g. `Shift+1` on a US
 * layout reports `key === '!'`, which a naive `/^[a-zA-Z0-9]$/` test on `key`
 * would reject entirely, silently dropping the base key from the captured
 * chord), while `code` still reliably identifies the physical digit/letter
 * key regardless of which modifiers are held. Modifiers and function keys are
 * read from `key`, which is already stable for them across modifier state. */
export function tokenFor(code: string, key: string): string | null {
  if (key in MODIFIER_KEY_NAMES) return MODIFIER_KEY_NAMES[key]
  if (/^F(?:[1-9]|1[0-9]|2[0-4])$/.test(key)) return key.toLowerCase()

  const letterMatch = /^Key([A-Z])$/.exec(code)
  if (letterMatch) return letterMatch[1].toLowerCase()

  const digitMatch = /^Digit([0-9])$/.exec(code)
  if (digitMatch) return digitMatch[1]

  // Fallback for events where `code` is missing/non-standard (e.g. synthetic
  // events in tests) but `key` is still a plain, unshifted letter or digit.
  if (/^[a-zA-Z0-9]$/.test(key)) return key.toLowerCase()

  return null
}

export function isModifierToken(token: string): token is ModifierToken {
  return (MODIFIER_ORDER as readonly string[]).includes(token)
}

/** Renders a set of tokens as the `+`-joined chord string, modifiers first in
 * a fixed order, then any base key. */
export function formatCombo(tokens: Set<string> | readonly string[]): string {
  const set = tokens instanceof Set ? tokens : new Set(tokens)
  const mods = MODIFIER_ORDER.filter((m) => set.has(m))
  const rest = [...set].filter((t) => !isModifierToken(t))
  return [...mods, ...rest].join('+')
}
