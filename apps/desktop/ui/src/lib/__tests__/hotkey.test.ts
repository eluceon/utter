import { describe, expect, it } from 'vitest'

import { formatCombo, isModifierToken, tokenFor } from '../hotkey'

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
