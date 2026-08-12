// The `notice` event, collected into a small list the settings window can
// render.
//
// This is the *second* place a notice reaches the user, not the first: the
// window this store feeds is closed for most of the app's life (dictation
// happens with nothing on screen but the HUD), so `src-tauri/src/sink.rs`
// puts every notice in front of the user as a desktop notification too, and
// that is the path a notice is guaranteed to travel. What this adds is a
// place to read one properly once the settings window *is* open: full
// wording, all of it still there after the desktop notification has faded,
// and dismissed only when the user says so.
//
// Plain store contract rather than runes, for the same reason as
// `stores.ts`: no component lifecycle here, and a `.ts` file is unit-testable
// with no Svelte compilation step.

import { writable, type Readable } from 'svelte/store'
import type { UnlistenFn } from '@tauri-apps/api/event'

import * as api from './api'
import type { NoticeKind, NoticePayload } from './types'

/** A notice on screen: the payload, plus what the list needs to track it. */
export interface Notice {
  id: number
  kind: NoticeKind
  message: string
  /** How many times this same message has arrived in a row (`1` for the
   * first). Repeats collapse into the one entry rather than stacking. */
  count: number
}

/**
 * How many notices are kept on screen at once. A degradation usually reports
 * one thing, so this is generous; the cap exists because the runtime is free
 * to report a *lot* (a speech engine that errors on every audio frame emits a
 * warning per frame), and an unbounded list would push the whole window's
 * content off the bottom of the screen.
 */
export const MAX_VISIBLE = 4

export interface NoticeStore extends Readable<Notice[]> {
  /** Starts listening for `notice` events. Resolves to the unlisten
   * function; call it when the window goes away. */
  start(): Promise<UnlistenFn>
  /** Adds a notice, as if one had arrived over the event bus. */
  push(payload: NoticePayload): void
  /** Removes the notice with `id`, if it is still on screen. */
  dismiss(id: number): void
}

export function createNoticeStore(backend: typeof api = api): NoticeStore {
  const { subscribe, update } = writable<Notice[]>([])
  let nextId = 1

  function push(payload: NoticePayload): void {
    update((current) => {
      const newest = current[current.length - 1]
      // A runtime that keeps reporting the same failure is reporting one
      // problem, not a hundred: count it, don't repeat it.
      if (newest && newest.kind === payload.kind && newest.message === payload.message) {
        const collapsed = { ...newest, count: newest.count + 1 }
        return [...current.slice(0, -1), collapsed]
      }
      const next = [...current, { id: nextId++, ...payload, count: 1 }]
      return next.slice(-MAX_VISIBLE)
    })
  }

  function dismiss(id: number): void {
    update((current) => current.filter((notice) => notice.id !== id))
  }

  function start(): Promise<UnlistenFn> {
    return backend.onNotice(push)
  }

  return { subscribe, start, push, dismiss }
}

/** The app-wide notice store, fed by the real `notice` event. */
export const noticeStore = createNoticeStore()
