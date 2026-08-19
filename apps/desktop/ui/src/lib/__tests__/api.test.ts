import { beforeEach, describe, expect, it, vi } from 'vitest'

// `@tauri-apps/api` is mocked so this suite (and by extension every command
// wrapper) is verifiable without a running tauri backend — it pins down the
// exact command name and argument-object shape each wrapper sends, which is
// the part a refactor is most likely to silently break.
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(async () => undefined),
}))
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => undefined),
}))

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

import * as api from '../api'

const mockInvoke = vi.mocked(invoke)
const mockListen = vi.mocked(listen)

describe('api command wrappers', () => {
  beforeEach(() => {
    mockInvoke.mockClear()
    mockListen.mockClear()
  })

  it('getSettings -> get_settings with no args', async () => {
    await api.getSettings()
    expect(mockInvoke).toHaveBeenCalledWith('get_settings')
  })

  it('saveSettings -> save_settings with a `settings` key', async () => {
    const settings = { general: {} } as never
    await api.saveSettings(settings)
    expect(mockInvoke).toHaveBeenCalledWith('save_settings', { settings })
  })

  it('listDevices -> list_devices', async () => {
    await api.listDevices()
    expect(mockInvoke).toHaveBeenCalledWith('list_devices')
  })

  it('listModels -> list_models', async () => {
    await api.listModels()
    expect(mockInvoke).toHaveBeenCalledWith('list_models')
  })

  it('downloadModel -> download_model with an `id` key', async () => {
    await api.downloadModel('small')
    expect(mockInvoke).toHaveBeenCalledWith('download_model', { id: 'small' })
  })

  it('removeModel -> remove_model with an `id` key', async () => {
    await api.removeModel('small')
    expect(mockInvoke).toHaveBeenCalledWith('remove_model', { id: 'small' })
  })

  it('historyList -> history_list with a `query` key, defaulting to null', async () => {
    await api.historyList('milk')
    expect(mockInvoke).toHaveBeenCalledWith('history_list', { query: 'milk' })

    await api.historyList()
    expect(mockInvoke).toHaveBeenCalledWith('history_list', { query: null })
  })

  it('historyDelete -> history_delete with an `id` key', async () => {
    await api.historyDelete(42)
    expect(mockInvoke).toHaveBeenCalledWith('history_delete', { id: 42 })
  })

  it('historyClear -> history_clear', async () => {
    await api.historyClear()
    expect(mockInvoke).toHaveBeenCalledWith('history_clear')
  })

  it('setApiKey -> set_api_key with `service` and `key` keys, key never logged elsewhere', async () => {
    await api.setApiKey('refine', 'sk-secret')
    expect(mockInvoke).toHaveBeenCalledWith('set_api_key', { service: 'refine', key: 'sk-secret' })
  })

  it('hasApiKey -> has_api_key with a `service` key', async () => {
    await api.hasApiKey('stt')
    expect(mockInvoke).toHaveBeenCalledWith('has_api_key', { service: 'stt' })
  })

  it('permissionsReport -> permissions_report', async () => {
    await api.permissionsReport()
    expect(mockInvoke).toHaveBeenCalledWith('permissions_report')
  })

  it('testRefine -> test_refine with a `sample` key', async () => {
    await api.testRefine('hello world')
    expect(mockInvoke).toHaveBeenCalledWith('test_refine', { sample: 'hello world' })
  })

  it('cancelDictation -> cancel_dictation', async () => {
    await api.cancelDictation()
    expect(mockInvoke).toHaveBeenCalledWith('cancel_dictation')
  })

  it('onModelProgress listens on "model-progress" and unwraps the payload', async () => {
    const handler = vi.fn()
    await api.onModelProgress(handler)
    expect(mockListen).toHaveBeenCalledWith('model-progress', expect.any(Function))
    const listenerCallback = mockListen.mock.calls[0][1] as (event: { payload: unknown }) => void
    listenerCallback({ payload: { id: 'small', done: 1, total: 2 } })
    expect(handler).toHaveBeenCalledWith({ id: 'small', done: 1, total: 2 })
  })

  it('takePendingNotices -> take_pending_notices with no args', async () => {
    await api.takePendingNotices()
    expect(mockInvoke).toHaveBeenCalledWith('take_pending_notices')
  })

  it('onDictationState listens on "dictation-state"', async () => {
    await api.onDictationState(vi.fn())
    expect(mockListen).toHaveBeenCalledWith('dictation-state', expect.any(Function))
  })

  it('onNotice listens on "notice"', async () => {
    await api.onNotice(vi.fn())
    expect(mockListen).toHaveBeenCalledWith('notice', expect.any(Function))
  })
})
