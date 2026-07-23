import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import * as api from '../api'
import { createSettingsStore, mergeDeep } from '../stores'
import { defaultSettings, type Settings } from '../types'

function get<T>(store: { subscribe: (fn: (value: T) => void) => () => void }): T {
  let value!: T
  const unsubscribe = store.subscribe((v) => {
    value = v
  })
  unsubscribe()
  return value
}

/** A `Promise` plus externally-callable `resolve`, for tests that need to
 * control exactly when an in-flight `saveSettings` call completes. */
function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((r) => {
    resolve = r
  })
  return { promise, resolve }
}

describe('mergeDeep', () => {
  it('merges nested objects without disturbing sibling fields', () => {
    const base = defaultSettings()
    const merged = mergeDeep(base, { general: { theme: 'dark' } })
    expect(merged.general.theme).toBe('dark')
    expect(merged.general.autostart).toBe(base.general.autostart)
    expect(merged.dictation).toEqual(base.dictation)
  })

  it('replaces arrays wholesale rather than merging elements', () => {
    const base = defaultSettings()
    const merged = mergeDeep(base, { dictionary: { terms: ['a', 'b'] } })
    expect(merged.dictionary.terms).toEqual(['a', 'b'])
  })

  it('does not mutate the base object', () => {
    const base = defaultSettings()
    mergeDeep(base, { general: { theme: 'dark' } })
    expect(base.general.theme).toBe('system')
  })
})

describe('settings store', () => {
  let backend: {
    getSettings: ReturnType<typeof vi.fn>
    saveSettings: ReturnType<typeof vi.fn>
  }

  beforeEach(() => {
    vi.useFakeTimers()
    backend = {
      getSettings: vi.fn(async () => defaultSettings()),
      saveSettings: vi.fn(async () => undefined),
    }
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('load() populates the store from the backend', async () => {
    const store = createSettingsStore(backend as unknown as typeof api)
    const loaded = await store.load()
    expect(loaded).toEqual(defaultSettings())
    expect(get(store)).toEqual(defaultSettings())
  })

  it('patch() throws if called before load()', () => {
    const store = createSettingsStore(backend as unknown as typeof api)
    expect(() => store.patch({ general: { theme: 'dark' } })).toThrow()
  })

  it('multiple rapid patches within the debounce window produce exactly one save', async () => {
    const store = createSettingsStore(backend as unknown as typeof api)
    await store.load()

    store.patch({ general: { theme: 'dark' } })
    await vi.advanceTimersByTimeAsync(100)
    store.patch({ general: { autostart: true } })
    await vi.advanceTimersByTimeAsync(100)
    store.patch({ dictation: { hud: false } })

    // Not yet 500ms since the last patch: no save should have fired.
    expect(backend.saveSettings).not.toHaveBeenCalled()

    await vi.advanceTimersByTimeAsync(500)

    expect(backend.saveSettings).toHaveBeenCalledTimes(1)
    const saved = backend.saveSettings.mock.calls[0][0] as Settings
    expect(saved.general.theme).toBe('dark')
    expect(saved.general.autostart).toBe(true)
    expect(saved.dictation.hud).toBe(false)
  })

  it('flush() saves immediately without waiting for the debounce timer', async () => {
    const store = createSettingsStore(backend as unknown as typeof api)
    await store.load()

    store.patch({ general: { theme: 'light' } })
    expect(backend.saveSettings).not.toHaveBeenCalled()

    await store.flush()

    expect(backend.saveSettings).toHaveBeenCalledTimes(1)
    expect((backend.saveSettings.mock.calls[0][0] as Settings).general.theme).toBe('light')
  })

  it('flush() with nothing pending is a no-op', async () => {
    const store = createSettingsStore(backend as unknown as typeof api)
    await store.load()
    await store.flush()
    expect(backend.saveSettings).not.toHaveBeenCalled()
  })

  it('a patch arriving while a save is in flight is coalesced into one follow-up save, not dropped', async () => {
    const first = deferred<void>()
    backend.saveSettings = vi
      .fn()
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(async () => undefined)

    const store = createSettingsStore(backend as unknown as typeof api)
    await store.load()

    store.patch({ general: { theme: 'dark' } })
    const flushed = store.flush()
    expect(backend.saveSettings).toHaveBeenCalledTimes(1)

    // A second patch shows up while the first save is still in flight, and
    // its own debounce timer subsequently elapses — still before the first
    // save resolves.
    store.patch({ general: { autostart: true } })
    await vi.advanceTimersByTimeAsync(500)

    // The in-flight save hasn't resolved yet, so the second patch must not
    // have fired a second, overlapping `saveSettings` call yet.
    expect(backend.saveSettings).toHaveBeenCalledTimes(1)

    first.resolve()
    await flushed

    expect(backend.saveSettings).toHaveBeenCalledTimes(2)
    const secondSave = backend.saveSettings.mock.calls[1][0] as Settings
    expect(secondSave.general.theme).toBe('dark')
    expect(secondSave.general.autostart).toBe(true)
  })
})
