import { describe, expect, it } from 'vitest'

import { previewModelOptions, previewModels } from '../models'
import type { ModelInfo } from '../types'

function model(id: string, engine: string): ModelInfo {
  return { id, engine, label: `${id} label`, size_mb: 1, installed: false }
}

/** One entry per engine string the catalog uses (`crates/utter-store/src/models.rs`), so a
 * filter that matched the wrong one has somewhere wrong to land. */
const CATALOG: ModelInfo[] = [
  model('small', 'whisper'),
  model('parakeet-tdt-110m-en', 'sherpa'),
  model('zipformer-ru-small', 'sherpa-streaming'),
  model('zipformer-en-small', 'sherpa-streaming'),
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
      { value: 'zipformer-en-small', label: 'zipformer-en-small label' },
    ])
  })

  it('offers "off" with an empty value even when no streaming model is catalogued', () => {
    // The empty value is what `Profiles.svelte` maps back to a `null` `draft` — the off state
    // has to stay reachable on a catalog with no streaming entries at all, or a profile could
    // never switch its preview back off.
    const options = previewModelOptions([model('small', 'whisper')])
    expect(options).toEqual([{ value: '', label: 'Off' }])
  })
})
