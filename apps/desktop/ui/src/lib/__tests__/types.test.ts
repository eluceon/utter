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
        hotkey: 'ctrl+alt+d',
        silence_timeout_secs: 5,
        hud: false,
      },
      engine: {
        active: 'cloud',
        whisper_model: 'medium',
        vosk_model: 'vosk-model-small-en-us-0.15',
        cloud: {
          base_url: 'https://api.groq.com/openai/v1',
          model: 'whisper-large-v3',
        },
      },
      refine: {
        enabled: true,
        tone: 'formal',
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
    }

    const roundTripped = JSON.parse(JSON.stringify(fixture)) as Settings
    expect(roundTripped).toEqual(fixture)
  })
})
