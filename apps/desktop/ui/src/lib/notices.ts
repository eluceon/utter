// The `notice` event, collected into a small list the settings window can
// render.
//
// Two channels carry a notice, and neither one alone carries every notice.
// `src-tauri/src/sink.rs` puts them in front of the user as desktop
// notifications, which is the only channel that works during dictation, when
// the window this store feeds is closed — but that channel is rate limited,
// and a notification held back there is dropped rather than deferred. This
// store is the other half: the full wording, still on screen after the
// notification has faded, dismissed only when the user says so, and — via
// `start()`, which drains the backend's parked queue — including the startup
// notices that were reported before any window was loaded to hear them.
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
  /** Starts listening for `notice` events *and* drains whatever the app
   * reported before this window existed. Resolves to the unlisten function;
   * call it when the window goes away. */
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

  // The app reports its startup conditions from Tauri's `setup`, which runs
  // before this webview is loaded, and `emit` has no replay — so the notices
  // that matter most (no model downloaded, an unavailable preview, a config
  // that would not migrate) are precisely the ones no listener can be
  // subscribed in time for. The backend parks them instead; this is where
  // they are collected.
  //
  // Failure is swallowed: a startup notice that cannot be fetched is not
  // worth costing the window its live subscription, which is the channel
  // everything after startup arrives on.
  async function drainPending(): Promise<void> {
    try {
      for (const payload of await backend.takePendingNotices()) {
        push(payload)
      }
    } catch {
      /* nothing to show, and nothing useful to say about it */
    }
  }

  async function start(): Promise<UnlistenFn> {
    // Subscribe before draining, not after: anything reported in between
    // would otherwise fall in the gap between the two, which is the same
    // shape of hole the parked queue exists to close.
    const unlisten = await backend.onNotice(push)
    await drainPending()
    return unlisten
  }

  return { subscribe, start, push, dismiss }
}

/** The app-wide notice store, fed by the real `notice` event. */
export const noticeStore = createNoticeStore()
