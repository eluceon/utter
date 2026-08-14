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
    const backend = fakeBackend({
      onNotice: vi.fn(async (handler: (payload: NoticePayload) => void) => {
        deliver = handler
        return unlisten
      }),
    })

    const store = createNoticeStore(backend)
    await expect(store.start()).resolves.toBe(unlisten)

    deliver?.({ kind: 'error', message: 'failed to start audio capture' })
    expect(get(store).map((n) => n.message)).toEqual(['failed to start audio capture'])
  })

  // The startup half, and the reason this store is the app's backstop at all.
  // The backend reports its startup conditions from Tauri's `setup`, before
  // this window is loaded, so the `notice` event above cannot carry them —
  // they are parked and handed over here. A store that merely subscribed
  // would leave them exactly as lost as they were before they were parked.
  //
  // Two of them, and the real pair: a transcription model that is not
  // downloaded together with a preview that is unavailable is one user's
  // actual configuration, and it is the second of the two that the
  // desktop-notification throttle drops.
  it('shows the notices the app parked before this window existed', async () => {
    const backend = fakeBackend({
      takePendingNotices: vi.fn(async () => [
        { kind: 'warning', message: 'no transcription model' } as NoticePayload,
        { kind: 'info', message: 'live preview unavailable' } as NoticePayload,
      ]),
    })

    const store = createNoticeStore(backend)
    await store.start()

    expect(get(store).map((n) => `${n.kind}: ${n.message}`)).toEqual([
      'warning: no transcription model',
      'info: live preview unavailable',
    ])
  })

  // Anything reported between the two calls would fall in the gap otherwise —
  // the same shape of hole the parked queue exists to close.
  it('subscribes before it drains, not after', async () => {
    const order: string[] = []
    const backend = fakeBackend({
      onNotice: vi.fn(async () => {
        order.push('subscribe')
        return vi.fn()
      }),
      takePendingNotices: vi.fn(async () => {
        order.push('drain')
        return []
      }),
    })

    await createNoticeStore(backend).start()

    expect(order).toEqual(['subscribe', 'drain'])
  })

  // A backend that cannot hand over its parked notices is a worse day than
  // usual, which is when the live subscription matters most: it must survive.
  it('keeps the live subscription when the parked queue cannot be drained', async () => {
    const unlisten = vi.fn()
    let deliver: ((payload: NoticePayload) => void) | undefined
    const backend = fakeBackend({
      onNotice: vi.fn(async (handler: (payload: NoticePayload) => void) => {
        deliver = handler
        return unlisten
      }),
      takePendingNotices: vi.fn(async () => {
        throw new Error('command not found')
      }),
    })

    const store = createNoticeStore(backend)
    await expect(store.start()).resolves.toBe(unlisten)

    deliver?.({ kind: 'warning', message: 'speech engine error: closed' })
    expect(get(store).map((n) => n.message)).toEqual(['speech engine error: closed'])
  })
})

/** The two backend calls `start()` makes, defaulting to "nothing happened". */
function fakeBackend(overrides: Partial<typeof api> = {}): typeof api {
  return {
    onNotice: vi.fn(async () => vi.fn()),
    takePendingNotices: vi.fn(async () => []),
    ...overrides,
  } as unknown as typeof api
}
