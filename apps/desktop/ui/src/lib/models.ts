// Selecting models out of the catalog `list_models` returns
// (`crates/utter-store/src/models.rs`), by the `engine` string each entry is
// catalogued under.

import type { ModelInfo } from './types'

/** The catalog `engine` string of the streaming models that drive the live
 * preview (`zipformer-*`), as opposed to `"sherpa"`, the offline models whose
 * text is actually inserted.
 *
 * The two are kept apart on purpose: a streaming model is small, fast and
 * emits no punctuation, which is fine for text that appears in the HUD while
 * you speak and wrong for text that lands in your editor. Nothing may offer a
 * `"sherpa-streaming"` entry where an engine model is chosen, or the other way
 * round. */
export const PREVIEW_ENGINE = 'sherpa-streaming'

/** Every streaming preview model in `models`, in catalog order. */
export function previewModels(models: ModelInfo[]): ModelInfo[] {
  return models.filter((m) => m.engine === PREVIEW_ENGINE)
}

/** The options of a profile's preview-model picker: the preview switched off
 * first (the default — an empty value the caller maps back to a `null`
 * `LanguageProfile.draft`), then one entry per streaming model.
 *
 * A model that is not installed yet is still offered, but says so: selecting
 * one is legal and saves fine, it simply produces no preview until the model
 * is downloaded on the Engines page *and* the app is restarted, since a
 * profile's engines are built once and cached for the run. Naming that up
 * front is what pushes the working order — download first, then select — for
 * a picker that otherwise looks like every option in it is ready to use. */
export function previewModelOptions(models: ModelInfo[]): { value: string; label: string }[] {
  return [
    { value: '', label: 'Off' },
    ...previewModels(models).map((m) => ({
      value: m.id,
      label: m.installed ? m.label : `${m.label} (not downloaded)`,
    })),
  ]
}
