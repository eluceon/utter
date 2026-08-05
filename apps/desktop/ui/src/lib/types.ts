// TypeScript mirror of `crates/utter-store/src/settings.rs` (and the small
// set of other Rust types the UI talks to). Field names and enum string
// values MUST match the Rust `serde` output exactly (snake_case field names;
// enums use `#[serde(rename_all = "snake_case")]` unless noted) — this file
// is the wire contract with `save_settings`/`get_settings`.

/** `crates/utter-store/src/settings.rs::Theme` */
export type Theme = 'system' | 'light' | 'dark'

/** `crates/utter-core/src/session.rs::DictationMode` */
export type DictationMode = 'push_to_talk' | 'toggle'

/** `crates/utter-store/src/settings.rs::EngineKind` */
export type EngineKind = 'whisper' | 'cloud' | 'sherpa'

/** `crates/utter-core/src/types.rs::Tone` */
export type Tone = 'verbatim' | 'clean' | 'formal' | 'notes' | 'code_comment'

/** `crates/utter-store/src/settings.rs::InjectionPreference` */
export type InjectionPreference = 'auto' | 'clipboard_paste' | 'type' | 'clipboard_only'

/** `crates/utter-store/src/settings.rs::General` */
export interface General {
  language: string | null
  theme: Theme
  autostart: boolean
}

/** `crates/utter-store/src/settings.rs::Dictation` */
export interface Dictation {
  mode: DictationMode
  hotkey: string
  silence_timeout_secs: number | null
  hud: boolean
}

/** `crates/utter-store/src/settings.rs::CloudSttCfg` */
export interface CloudSttCfg {
  base_url: string
  model: string
}

/** `crates/utter-store/src/settings.rs::EngineCfg` */
export interface EngineCfg {
  active: EngineKind
  whisper_model: string
  sherpa_model: string | null
  cloud: CloudSttCfg
}

/** `crates/utter-store/src/settings.rs::RefineCfg`. Master switch plus the
 * provider connection only — `tone` moved to `RefinePolicy.tone` (Task 16),
 * one profile at a time rather than one global setting. */
export interface RefineCfg {
  enabled: boolean
  base_url: string
  model: string
  timeout_secs: number
}

/** `crates/utter-store/src/profile.rs::RefinePolicy` */
export interface RefinePolicy {
  enabled: boolean
  tone: Tone
}

/** `crates/utter-store/src/profile.rs::DraftCfg` */
export interface DraftCfg {
  model: string
}

/** `crates/utter-store/src/profile.rs::LanguageProfile`. One hotkey chord
 * and everything dictating in it implies: which engine transcribes, which
 * model it loads, and whether the transcript is refined afterwards. */
export interface LanguageProfile {
  id: string
  hotkey: string
  language: string
  engine: EngineCfg
  draft: DraftCfg | null
  refine: RefinePolicy
}

/** `crates/utter-refine/src/rules.rs::ReplaceRule` */
export interface ReplaceRule {
  heard: string
  write: string
}

/** `crates/utter-store/src/settings.rs::Dictionary` */
export interface Dictionary {
  terms: string[]
  rules: ReplaceRule[]
}

/** `crates/utter-refine/src/snippets.rs::Snippet` */
export interface Snippet {
  trigger: string
  body: string
}

/** `crates/utter-store/src/settings.rs::HistoryCfg` */
export interface HistoryCfg {
  enabled: boolean
}

/** `crates/utter-store/src/settings.rs::Advanced` */
export interface Advanced {
  injection: InjectionPreference
  audio_device: string | null
  vad_sensitivity: number
  log_level: string
}

/** `crates/utter-store/src/settings.rs::Settings` */
export interface Settings {
  general: General
  dictation: Dictation
  engine: EngineCfg
  refine: RefineCfg
  dictionary: Dictionary
  snippets: Snippet[]
  history: HistoryCfg
  advanced: Advanced
  /** One entry per language the user dictates in, each binding a hotkey to
   * an engine, a model and a refinement policy. Carried since Task 13;
   * `defaultSettings()` below MUST seed exactly the one profile
   * `Settings::default()` does — `App.svelte`'s onboarding gate compares a
   * freshly loaded `Settings` against this function's output key-for-key
   * (see `deepEqual`), so a missing or mismatched profile here makes
   * onboarding never show on a fresh install. */
  profiles: LanguageProfile[]
}

/** Mirrors `Settings::default()` in `crates/utter-store/src/settings.rs`. */
export function defaultSettings(): Settings {
  return {
    general: {
      language: null,
      theme: 'system',
      autostart: false,
    },
    dictation: {
      mode: 'push_to_talk',
      hotkey: 'ctrl+super',
      silence_timeout_secs: null,
      hud: true,
    },
    engine: {
      active: 'whisper',
      whisper_model: 'small',
      sherpa_model: null,
      cloud: {
        base_url: 'https://api.openai.com/v1',
        model: 'whisper-1',
      },
    },
    refine: {
      enabled: false,
      base_url: 'http://localhost:11434/v1',
      model: 'llama3.2',
      timeout_secs: 10,
    },
    dictionary: {
      terms: [],
      rules: [],
    },
    snippets: [],
    history: {
      enabled: true,
    },
    advanced: {
      injection: 'auto',
      audio_device: null,
      vad_sensitivity: 0.5,
      log_level: 'info',
    },
    // Mirrors `Settings::default()`'s single seeded profile exactly: a
    // fresh install gets one profile on the local sherpa-onnx engine (see
    // `EngineCfg::sherpa("parakeet-tdt-110m-en")` in
    // `crates/utter-store/src/settings.rs`), refinement off (the sherpa
    // models already emit punctuation and casing), and no draft/streaming
    // model configured yet.
    profiles: [
      {
        id: 'default',
        hotkey: 'ctrl+super',
        language: 'en',
        engine: {
          active: 'sherpa',
          whisper_model: 'small',
          sherpa_model: 'parakeet-tdt-110m-en',
          cloud: {
            base_url: 'https://api.openai.com/v1',
            model: 'whisper-1',
          },
        },
        draft: null,
        refine: {
          enabled: false,
          tone: 'clean',
        },
      },
    ],
  }
}

/** Structural deep equality: object key order never matters (unlike a naive
 * `JSON.stringify` comparison, which is sensitive to it), arrays compare
 * element-by-element in order, and primitives compare with `===`. Sufficient
 * for comparing two `Settings` values without depending on both having been
 * serialized with identical key ordering. */
export function deepEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true
  if (typeof a !== typeof b || a === null || b === null) return false

  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false
    return a.every((value, i) => deepEqual(value, b[i]))
  }

  if (typeof a === 'object' && typeof b === 'object') {
    const aRecord = a as Record<string, unknown>
    const bRecord = b as Record<string, unknown>
    const aKeys = Object.keys(aRecord)
    const bKeys = Object.keys(bRecord)
    if (aKeys.length !== bKeys.length) return false
    return aKeys.every(
      (key) =>
        Object.prototype.hasOwnProperty.call(bRecord, key) &&
        deepEqual(aRecord[key], bRecord[key]),
    )
  }

  return false
}

/** `crates/utter-store/src/models.rs::ModelInfo` */
export interface ModelInfo {
  id: string
  engine: string
  label: string
  size_mb: number
  installed: boolean
}

/** `crates/utter-store/src/history.rs::HistoryEntry` */
export interface HistoryEntry {
  id: number
  created_at: number
  duration_ms: number
  engine: string
  raw_text: string
  final_text: string
  app: string | null
}

/** `crates/utter-inject/src/hotkey.rs::PermissionReport` */
export interface PermissionReport {
  input_group: boolean
  uinput_writable: boolean
  fix_command: string
}

/** Payload of the `model-progress` event (`src-tauri/src/events.rs::ModelProgress`). */
export interface ModelProgressPayload {
  id: string
  done: number
  total: number
}

/** `src-tauri/src/events.rs::DictationPhase` */
export type DictationPhase = 'idle' | 'recording' | 'transcribing' | 'refining' | 'injecting'

/** Payload of the `dictation-state` event (`src-tauri/src/events.rs::DictationState`). */
export interface DictationStatePayload {
  state: DictationPhase
  level: number
  partial: string | null
}

/** `src-tauri/src/events.rs::NoticeKind` */
export type NoticeKind = 'info' | 'warning' | 'error'

/** Payload of the `notice` event (`src-tauri/src/events.rs::Notice`). */
export interface NoticePayload {
  kind: NoticeKind
  message: string
}

/** The two api-key "service" identities `set_api_key`/`has_api_key` accept
 * (`STT_KEY_SERVICE` / `REFINE_KEY_SERVICE` in `src-tauri/src/lib.rs`). */
export type ApiKeyService = 'stt' | 'refine'
