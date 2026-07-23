// The settings store: loads `Settings` once, lets pages `patch()` partial
// changes, and persists them via `save_settings` on a trailing debounce so
// rapid edits (a slider being dragged, a text field being typed into) don't
// each fire their own save.
//
// Uses Svelte's plain store contract (`subscribe`/`writable`) rather than
// runes: this file has no component lifecycle, and the store contract keeps
// it trivially unit-testable under vitest with no Svelte compilation step
// (a `.ts` file gets no rune transform — only `.svelte`/`.svelte.ts` files
// do), while components can still consume it with `$settingsStore`.

import { writable, type Readable } from 'svelte/store'

import * as api from './api'
import type { Settings } from './types'

/** Same shape as `T`, but every property at every level is optional — what a
 * caller may pass to `patch()`. Arrays and other non-plain-object values are
 * replaced wholesale rather than merged element-by-element. */
export type DeepPartial<T> = {
  [K in keyof T]?: T[K] extends (infer U)[]
    ? U[]
    : T[K] extends object
      ? DeepPartial<T[K]>
      : T[K]
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/** Recursively merges `patch` onto `base`, returning a new object. Plain
 * objects are merged key-by-key; arrays, primitives, and `null` from `patch`
 * replace the corresponding value in `base` outright. */
export function mergeDeep<T extends object>(base: T, patch: DeepPartial<T>): T {
  const baseRecord = base as Record<string, unknown>
  const patchRecord = patch as Record<string, unknown>
  const result: Record<string, unknown> = { ...baseRecord }
  for (const key of Object.keys(patchRecord)) {
    const patchValue = patchRecord[key]
    const baseValue = baseRecord[key]
    if (isPlainObject(patchValue) && isPlainObject(baseValue)) {
      result[key] = mergeDeep(baseValue, patchValue)
    } else if (patchValue !== undefined) {
      result[key] = patchValue
    }
  }
  return result as T
}

const DEBOUNCE_MS = 500

export interface SettingsStore extends Readable<Settings | null> {
  /** Loads settings from the backend, replacing any local state. */
  load(): Promise<Settings>
  /** Merges `partial` into the current settings and schedules a debounced
   * save. Throws if called before `load()` has resolved. */
  patch(partial: DeepPartial<Settings>): void
  /** Cancels any pending debounce timer and saves immediately, if there is
   * anything pending. Call this before navigating away or on window unload
   * so a patch made just before switching pages is never dropped. Safe to
   * call with nothing pending (a no-op). Awaiting it also waits out any save
   * already in flight, including one more round if a patch arrived while
   * that save was in flight. */
  flush(): Promise<void>
}

export function createSettingsStore(backend: typeof api = api): SettingsStore {
  const { subscribe, set, update } = writable<Settings | null>(null)

  let saveTimer: ReturnType<typeof setTimeout> | null = null
  /** The most recent not-yet-sent settings snapshot, or `null` if nothing is
   * pending (either nothing changed since the last save, or a save already
   * picked it up). */
  let pending: Settings | null = null
  /** The in-flight `save_settings` call, if any — used so a `patch()` that
   * arrives while a save is running doesn't fire a second, overlapping call;
   * it instead waits and re-saves once the first completes. */
  let saving: Promise<void> | null = null
  let saveAgainAfterCurrent = false

  async function load(): Promise<Settings> {
    const loaded = await backend.getSettings()
    set(loaded)
    return loaded
  }

  function patch(partial: DeepPartial<Settings>): void {
    update((current) => {
      if (!current) {
        throw new Error('settingsStore.patch called before load()')
      }
      const next = mergeDeep(current, partial)
      pending = next
      if (saveTimer) clearTimeout(saveTimer)
      saveTimer = setTimeout(() => {
        saveTimer = null
        void runSave()
      }, DEBOUNCE_MS)
      return next
    })
  }

  /** Sends whatever is currently `pending`, coalescing any patch that shows
   * up while the send is in flight into exactly one follow-up save rather
   * than one per patch. */
  function runSave(): Promise<void> {
    if (saving) {
      saveAgainAfterCurrent = true
      return saving
    }
    if (!pending) {
      return Promise.resolve()
    }
    const toSave = pending
    pending = null

    saving = backend
      .saveSettings(toSave)
      .finally(() => {
        saving = null
        if (saveAgainAfterCurrent) {
          saveAgainAfterCurrent = false
          return runSave()
        }
        return undefined
      })
      .then(() => undefined)

    return saving
  }

  async function flush(): Promise<void> {
    if (saveTimer) {
      clearTimeout(saveTimer)
      saveTimer = null
    }
    await runSave()
  }

  return { subscribe, load, patch, flush }
}

/** The app-wide settings store, backed by the real tauri commands. */
export const settingsStore = createSettingsStore()
