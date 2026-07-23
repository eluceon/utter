// Typed wrappers around every tauri command the settings UI talks to (see
// `apps/desktop/src-tauri/src/commands.rs`, the contract this file mirrors),
// plus typed `listen()` helpers for the events it emits.
//
// Every wrapper is a thin, individually-mockable function (`vi.mock` this
// whole module in tests) rather than a class, so the rest of the UI never
// imports `@tauri-apps/api` directly.
//
// Argument naming: tauri v2 converts a Rust command's snake_case parameter
// names to camelCase JS object keys by default (unless the command opts into
// `rename_all = "snake_case"`, which none of these do). Every argument name
// below is already a single word (`id`, `settings`, `query`, `service`,
// `key`, `sample`), so camelCase and snake_case coincide — there is no
// renaming to get wrong here, but the invoke payload keys must still match
// the Rust parameter names, not the JSON body's own field names.

import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

import type {
  ApiKeyService,
  DictationStatePayload,
  HistoryEntry,
  ModelInfo,
  ModelProgressPayload,
  NoticePayload,
  PermissionReport,
  Settings,
} from './types'

export function getSettings(): Promise<Settings> {
  return invoke('get_settings')
}

export function saveSettings(settings: Settings): Promise<void> {
  return invoke('save_settings', { settings })
}

export function listDevices(): Promise<string[]> {
  return invoke('list_devices')
}

export function listModels(): Promise<ModelInfo[]> {
  return invoke('list_models')
}

export function downloadModel(id: string): Promise<void> {
  return invoke('download_model', { id })
}

export function removeModel(id: string): Promise<void> {
  return invoke('remove_model', { id })
}

export function historyList(query?: string): Promise<HistoryEntry[]> {
  return invoke('history_list', { query: query ?? null })
}

export function historyDelete(id: number): Promise<void> {
  return invoke('history_delete', { id })
}

export function historyClear(): Promise<void> {
  return invoke('history_clear')
}

export function setApiKey(service: ApiKeyService, key: string): Promise<void> {
  return invoke('set_api_key', { service, key })
}

export function hasApiKey(service: ApiKeyService): Promise<boolean> {
  return invoke('has_api_key', { service })
}

export function permissionsReport(): Promise<PermissionReport> {
  return invoke('permissions_report')
}

export function testRefine(sample: string): Promise<string> {
  return invoke('test_refine', { sample })
}

export function cancelDictation(): Promise<void> {
  return invoke('cancel_dictation')
}

export function onModelProgress(
  handler: (payload: ModelProgressPayload) => void,
): Promise<UnlistenFn> {
  return listen<ModelProgressPayload>('model-progress', (event) => handler(event.payload))
}

export function onDictationState(
  handler: (payload: DictationStatePayload) => void,
): Promise<UnlistenFn> {
  return listen<DictationStatePayload>('dictation-state', (event) => handler(event.payload))
}

export function onNotice(handler: (payload: NoticePayload) => void): Promise<UnlistenFn> {
  return listen<NoticePayload>('notice', (event) => handler(event.payload))
}
