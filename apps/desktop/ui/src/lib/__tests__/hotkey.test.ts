import { describe, expect, it } from 'vitest'

import { chordsConflict, formatCombo, isModifierToken, parseChordTokens, tokenFor } from '../hotkey'

describe('tokenFor', () => {
  it('reads plain letters and digits from `key` when `code` is absent', () => {
    expect(tokenFor('', 'd')).toBe('d')
    expect(tokenFor('', '5')).toBe('5')
  })

  it('reads letters and digits from `code`, unaffected by an unshifted `key`', () => {
    expect(tokenFor('KeyD', 'd')).toBe('d')
    expect(tokenFor('Digit5', '5')).toBe('5')
  })

  it('shift+digit: derives the digit from `code` even though `key` is the shifted symbol', () => {
    // Shift+1 on a US layout: event.key === '!', event.code === 'Digit1'.
    expect(tokenFor('Digit1', '!')).toBe('1')
  })

  it('shift+letter: derives the letter from `code` even though `key` is uppercase', () => {
    expect(tokenFor('KeyD', 'D')).toBe('d')
  })

  it('recognizes modifier keys regardless of `code`', () => {
    expect(tokenFor('ControlLeft', 'Control')).toBe('ctrl')
    expect(tokenFor('AltLeft', 'Alt')).toBe('alt')
    expect(tokenFor('ShiftLeft', 'Shift')).toBe('shift')
    expect(tokenFor('MetaLeft', 'Meta')).toBe('super')
  })

  it('recognizes Super via WebKitGTK\'s non-standard `key`/`code` values', () => {
    // Confirmed against a real WebKitGTK 2.50 webview (the Linux desktop
    // build's actual engine): it reports `key: 'Super'` and
    // `code: 'OSLeft'/'OSRight'` for the Super key instead of the UI Events
    // spec's `'Meta'`/`'MetaLeft'`/`'MetaRight'`.
    expect(tokenFor('OSLeft', 'Super')).toBe('super')
    expect(tokenFor('OSRight', 'Super')).toBe('super')
  })

  it('recognizes Super from `code` alone, e.g. if `key` is ever unrecognized', () => {
    expect(tokenFor('MetaRight', 'SomethingUnexpected')).toBe('super')
  })

  it('recognizes function keys F1..F24', () => {
    expect(tokenFor('F1', 'F1')).toBe('f1')
    expect(tokenFor('F24', 'F24')).toBe('f24')
  })

  it('recognizes the space bar from `code`', () => {
    expect(tokenFor('Space', ' ')).toBe('space')
  })

  it('recognizes the space bar from `key` when `code` is absent', () => {
    expect(tokenFor('', ' ')).toBe('space')
  })

  it('rejects keys outside the accepted grammar', () => {
    expect(tokenFor('Escape', 'Escape')).toBeNull()
    expect(tokenFor('Tab', 'Tab')).toBeNull()
    expect(tokenFor('', '!')).toBeNull()
  })
})

describe('isModifierToken', () => {
  it('accepts exactly the four modifier tokens', () => {
    expect(isModifierToken('ctrl')).toBe(true)
    expect(isModifierToken('alt')).toBe(true)
    expect(isModifierToken('shift')).toBe(true)
    expect(isModifierToken('super')).toBe(true)
    expect(isModifierToken('d')).toBe(false)
  })
})

describe('formatCombo', () => {
  it('orders modifiers first in a fixed order, then the base key', () => {
    expect(formatCombo(new Set(['shift', 'ctrl', '1']))).toBe('ctrl+shift+1')
  })

  it('supports a modifier-only chord', () => {
    expect(formatCombo(new Set(['super', 'ctrl']))).toBe('ctrl+super')
  })
})

describe('parseChordTokens', () => {
  it('lowercases and trims tokens', () => {
    expect(parseChordTokens('Ctrl+ Super ')).toEqual(new Set(['ctrl', 'super']))
  })

  it('treats a chord with no tokens as unparseable, mirroring HotkeyParseError::Empty', () => {
    expect(parseChordTokens('')).toBeNull()
    expect(parseChordTokens('+')).toBeNull()
    expect(parseChordTokens('   ')).toBeNull()
  })
})

// Mirrors `chords_that_can_complete_together_conflict_but_nested_ones_do_not`
// in `crates/utter-inject/src/hotkey.rs`, token-for-token, so the two
// implementations of "conflict" are checked against the same fixture chords
// rather than against independently-invented ones.
describe('chordsConflict', () => {
  const tokens = (chord: string) => parseChordTokens(chord)!

  it('identical chords conflict', () => {
    const a = tokens('ctrl+super')
    const b = tokens('ctrl+super')
    expect(chordsConflict(a, b)).toBe(true)
  })

  it('a nested chord does not conflict with the shorter chord it contains', () => {
    // Completing ctrl+alt+super still needs `alt`, which ctrl+super does not
    // require at all, so the two stay distinguishable.
    const shorter = tokens('ctrl+super')
    const nested = tokens('ctrl+alt+super')
    expect(chordsConflict(shorter, nested)).toBe(false)
  })

  it('two chords that overlap without either containing the other conflict', () => {
    // Holding ctrl+alt and then pressing super completes both ctrl+alt and
    // alt+super at once.
    const partner = tokens('ctrl+alt')
    const overlapping = tokens('alt+super')
    expect(chordsConflict(partner, overlapping)).toBe(true)
  })

  it('disjoint chords do not conflict', () => {
    expect(chordsConflict(tokens('ctrl+super'), tokens('alt+shift+d'))).toBe(false)
  })
})
