import { describe, expect, it } from 'vitest'

import { deepEqual, defaultSettings, type Settings } from '../types'

describe('deepEqual', () => {
  it('is true for structurally equal objects regardless of key order, false for a real difference', () => {
    const a = { general: { theme: 'dark', autostart: true }, snippets: [{ trigger: 't', body: 'b' }] }
    const b = { snippets: [{ body: 'b', trigger: 't' }], general: { autostart: true, theme: 'dark' } }
    expect(deepEqual(a, b)).toBe(true)
    expect(deepEqual(a, { ...b, general: { ...b.general, theme: 'light' } })).toBe(false)
  })
})

describe('Settings type/JSON round-trip', () => {
  it('defaultSettings() survives a JSON round-trip unchanged', () => {
    const settings = defaultSettings()
    const roundTripped = JSON.parse(JSON.stringify(settings)) as Settings
    expect(roundTripped).toEqual(settings)
  })

  it('a fully-populated fixture (every field non-default) survives a JSON round-trip unchanged', () => {
    // Every field set to a non-default value, using only values the Rust
    // side's serde (de)serialization actually produces — this is what
    // catches a typo'd field name or wrong enum string surviving `npm run
    // check` (TS structural typing wouldn't catch a *value* mismatch, only a
    // missing/extra field), since a stray key would still round-trip fine
    // but a Settings fixture below is annotated with the `Settings` type so
    // TS enforces every field is present and correctly named/typed.
    const fixture: Settings = {
      general: {
        language: 'en',
        theme: 'dark',
        autostart: true,
      },
      dictation: {
        mode: 'toggle',
        silence_timeout_secs: 5,
        hud: false,
      },
      refine: {
        enabled: true,
        base_url: 'https://api.openai.com/v1',
        model: 'gpt-4o-mini',
        timeout_secs: 30,
      },
      dictionary: {
        terms: ['SQLite', 'Kubernetes'],
        rules: [{ heard: 'my sequel', write: 'MySQL' }],
      },
      snippets: [{ trigger: 'insert my email signature', body: 'Best, Dima' }],
      history: {
        enabled: false,
      },
      advanced: {
        injection: 'clipboard_only',
        audio_device: 'USB Microphone',
        vad_sensitivity: 0.75,
        log_level: 'debug',
      },
      // Two profiles (the normal bilingual case) with every field set to a
      // non-default value, including two profiles that both sit on the
      // sherpa engine — the case `ProfileDeps.engine_label` alone cannot
      // distinguish, so the wire contract needs `id` to survive intact for
      // each.
      profiles: [
        {
          id: 'ru',
          hotkey: 'ctrl+super',
          language: 'ru',
          engine: {
            active: 'sherpa',
            whisper_model: 'small',
            sherpa_model: 'gigaam-v3-e2e-rnnt',
            cloud: {
              base_url: 'https://api.openai.com/v1',
              model: 'whisper-1',
            },
          },
          draft: { model: 'zipformer-ru-small' },
          refine: { enabled: false, tone: 'clean' },
        },
        {
          id: 'en',
          hotkey: 'ctrl+alt+super',
          language: 'en',
          engine: {
            active: 'whisper',
            whisper_model: 'medium',
            sherpa_model: null,
            cloud: {
              base_url: 'https://api.openai.com/v1',
              model: 'whisper-1',
            },
          },
          draft: null,
          refine: { enabled: true, tone: 'formal' },
        },
      ],
    }

    const roundTripped = JSON.parse(JSON.stringify(fixture)) as Settings
    expect(roundTripped).toEqual(fixture)
  })
})

describe('defaultSettings', () => {
  // Deliberately does NOT build its expectation from `defaultSettings()`
  // itself (see the fixture test above's sibling trap: a round-trip test
  // whose input is `defaultSettings()` agrees with itself no matter how
  // wrong `defaultSettings()` is). `expected` below is instead a plain
  // object hand-written to match `Settings::default()` in
  // `crates/utter-store/src/settings.rs`, independent of this file's own
  // `defaultSettings()` implementation — so if `defaultSettings()` drifts
  // from the Rust default (missing the `profiles` seed, wrong field count,
  // wrong value), this test fails on its own rather than agreeing with the
  // bug. This is the gap that let `App.svelte`'s onboarding gate
  // (`deepEqual(settings, defaultSettings())`) silently break: real
  // settings from the backend carry a `profiles` key that `defaultSettings()`
  // didn't, so `isDefaultSettings` was always false and a fresh install
  // never saw onboarding.
  it('matches Settings::default() field-for-field, including its one seeded profile', () => {
    const expected = {
      general: { language: null, theme: 'system', autostart: false },
      dictation: { mode: 'push_to_talk', silence_timeout_secs: null, hud: true },
      refine: {
        enabled: false,
        base_url: 'http://localhost:11434/v1',
        model: 'llama3.2',
        timeout_secs: 10,
      },
      dictionary: { terms: [], rules: [] },
      snippets: [],
      history: { enabled: true },
      advanced: {
        injection: 'auto',
        audio_device: null,
        vad_sensitivity: 0.5,
        log_level: 'info',
      },
      profiles: [
        {
          id: 'default',
          hotkey: 'ctrl+super',
          language: 'en',
          engine: {
            active: 'sherpa',
            whisper_model: 'small',
            sherpa_model: 'parakeet-tdt-110m-en',
            cloud: { base_url: 'https://api.openai.com/v1', model: 'whisper-1' },
          },
          draft: null,
          refine: { enabled: false, tone: 'clean' },
        },
      ],
    }

    expect(deepEqual(defaultSettings(), expected)).toBe(true)
  })

  // Pins the exact property App.svelte's onboarding gate relies on:
  // `deepEqual` compares key counts (see its own doc comment), so a
  // freshly-loaded `Settings` value that is genuinely still "the defaults"
  // must compare equal to `defaultSettings()` after a JSON round-trip (the
  // same transformation `get_settings`'s payload goes through) — not merely
  // by construction.
  it('is deepEqual to itself after a JSON round-trip, the same shape a fresh install loads as', () => {
    const roundTripped = JSON.parse(JSON.stringify(defaultSettings())) as Settings
    expect(deepEqual(roundTripped, defaultSettings())).toBe(true)
  })
})
