import { describe, expect, it } from 'vitest'

import { previewModelOptions, previewModels } from '../models'
import type { ModelInfo } from '../types'

/** `installed` is required rather than defaulted, because the picker's labels now depend on it:
 * a fixture that quietly left every model uninstalled would make an "installed" assertion
 * unwritable and an "uninstalled" one pass for no reason. */
function model(id: string, engine: string, installed: boolean): ModelInfo {
  return { id, engine, label: `${id} label`, size_mb: 1, installed }
}

/** One entry per engine string the catalog uses (`crates/utter-store/src/models.rs`), so a
 * filter that matched the wrong one has somewhere wrong to land. The two streaming entries sit
 * on opposite sides of `installed` for the same reason. */
const CATALOG: ModelInfo[] = [
  model('small', 'whisper', true),
  model('parakeet-tdt-110m-en', 'sherpa', true),
  model('zipformer-ru-small', 'sherpa-streaming', true),
  model('zipformer-en-small', 'sherpa-streaming', false),
]

describe('previewModels', () => {
  it('selects the streaming models and nothing else', () => {
    // The offline `sherpa` entry is the one this must never return: it is the engine whose text
    // actually gets inserted, and offering it as a preview model (or a preview model as an
    // engine) is exactly what the two distinct engine strings exist to prevent.
    expect(previewModels(CATALOG).map((m) => m.id)).toEqual([
      'zipformer-ru-small',
      'zipformer-en-small',
    ])
  })
})

describe('previewModelOptions', () => {
  it('offers "off" first, then one option per streaming model', () => {
    expect(previewModelOptions(CATALOG)).toEqual([
      { value: '', label: 'Off' },
      { value: 'zipformer-ru-small', label: 'zipformer-ru-small label' },
      { value: 'zipformer-en-small', label: 'zipformer-en-small label (not downloaded)' },
    ])
  })

  it('marks an uninstalled model and leaves an installed one alone', () => {
    // Selecting a model that is not downloaded yet is a silent dead end: it saves fine and then
    // produces no preview and no error, because the profile's engines were already built.
    // Labelling it is the only warning the picker gives, so both branches are asserted here
    // rather than only the one the shared fixture happens to exercise.
    const labels = previewModelOptions(CATALOG).map((o) => o.label)

    expect(labels).toContain('zipformer-en-small label (not downloaded)')
    expect(labels).toContain('zipformer-ru-small label')
    expect(labels).not.toContain('zipformer-ru-small label (not downloaded)')
  })

  it('offers "off" with an empty value even when no streaming model is catalogued', () => {
    // The empty value is what `Profiles.svelte` maps back to a `null` `draft` — the off state
    // has to stay reachable on a catalog with no streaming entries at all, or a profile could
    // never switch its preview back off.
    const options = previewModelOptions([model('small', 'whisper', true)])
    expect(options).toEqual([{ value: '', label: 'Off' }])
  })
})
