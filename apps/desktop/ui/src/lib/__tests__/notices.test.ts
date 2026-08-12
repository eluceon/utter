import { get } from 'svelte/store'
import { describe, expect, it, vi } from 'vitest'

import * as api from '../api'
import { MAX_VISIBLE, createNoticeStore } from '../notices'
import type { NoticePayload } from '../types'

describe('notice store', () => {
  it('keeps a notice that arrives', () => {
    const store = createNoticeStore()
    store.push({ kind: 'info', message: 'live preview unavailable' })

    expect(get(store)).toEqual([
      { id: 1, kind: 'info', message: 'live preview unavailable', count: 1 },
    ])
  })

  // The runtime reports some conditions once per audio frame. Stacking one
  // entry per report would bury every other notice under the same sentence.
  it('collapses a message repeated back to back into one entry', () => {
    const store = createNoticeStore()
    const same: NoticePayload = { kind: 'warning', message: 'speech engine error: closed' }
    store.push(same)
    store.push(same)
    store.push(same)

    expect(get(store)).toEqual([{ id: 1, ...same, count: 3 }])
  })

  // Only *consecutive* repeats collapse: the same warning after something
  // else happened is news again, and folding it into an entry the user has
  // already read would hide it.
  it('does not collapse a message that something else interrupted', () => {
    const store = createNoticeStore()
    store.push({ kind: 'warning', message: 'engine missing' })
    store.push({ kind: 'info', message: 'live preview unavailable' })
    store.push({ kind: 'warning', message: 'engine missing' })

    expect(get(store).map((n) => n.message)).toEqual([
      'engine missing',
      'live preview unavailable',
      'engine missing',
    ])
  })

  it('does not collapse the same words reported at a different severity', () => {
    const store = createNoticeStore()
    store.push({ kind: 'warning', message: 'no profile configured' })
    store.push({ kind: 'error', message: 'no profile configured' })

    expect(get(store).map((n) => n.kind)).toEqual(['warning', 'error'])
  })

  it('keeps only the newest notices once the list is full', () => {
    const store = createNoticeStore()
    for (let i = 0; i < MAX_VISIBLE + 2; i += 1) {
      store.push({ kind: 'info', message: `notice ${i}` })
    }

    const visible = get(store)
    expect(visible).toHaveLength(MAX_VISIBLE)
    expect(visible[0].message).toBe('notice 2')
    expect(visible[MAX_VISIBLE - 1].message).toBe(`notice ${MAX_VISIBLE + 1}`)
  })

  it('removes exactly the dismissed notice', () => {
    const store = createNoticeStore()
    store.push({ kind: 'info', message: 'first' })
    store.push({ kind: 'info', message: 'second' })

    const [first] = get(store)
    store.dismiss(first.id)

    expect(get(store).map((n) => n.message)).toEqual(['second'])
  })

  it('ignores a dismiss for a notice that is no longer there', () => {
    const store = createNoticeStore()
    store.push({ kind: 'info', message: 'first' })
    store.dismiss(999)

    expect(get(store)).toHaveLength(1)
  })

  // The whole defect this store fixes was a `notice` listener nobody ever
  // subscribed: the wiring is the part worth pinning.
  it('subscribes to the notice event and shows what it delivers', async () => {
    const unlisten = vi.fn()
    let deliver: ((payload: NoticePayload) => void) | undefined
    const backend = {
      onNotice: vi.fn(async (handler: (payload: NoticePayload) => void) => {
        deliver = handler
        return unlisten
      }),
    } as unknown as typeof api

    const store = createNoticeStore(backend)
    await expect(store.start()).resolves.toBe(unlisten)

    deliver?.({ kind: 'error', message: 'failed to start audio capture' })
    expect(get(store).map((n) => n.message)).toEqual(['failed to start audio capture'])
  })
})
