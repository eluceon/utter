<script lang="ts">
  import { onDestroy, onMount } from 'svelte'

  import Section from '../lib/components/Section.svelte'
  import Field from '../lib/components/Field.svelte'
  import Select from '../lib/components/Select.svelte'
  import TextInput from '../lib/components/TextInput.svelte'
  import * as api from '../lib/api'
  import { settingsStore } from '../lib/stores'
  import type { EngineKind, ModelInfo } from '../lib/types'

  let settings = $derived($settingsStore!)

  let models = $state<ModelInfo[]>([])
  let modelsError = $state('')
  let progress = $state<Record<string, { done: number; total: number }>>({})
  let busy = $state<Record<string, boolean>>({})
  let sttConfigured = $state(false)
  let sttApiKey = $state('')
  let sttKeyJustSaved = $state(false)
  let sttKeyError = $state('')

  const ENGINE_OPTIONS: { value: EngineKind; label: string }[] = [
    { value: 'whisper', label: 'Whisper (local)' },
    { value: 'vosk', label: 'Vosk (local)' },
    { value: 'cloud', label: 'Cloud (OpenAI-compatible)' },
  ]

  let whisperModels = $derived(models.filter((m) => m.engine === 'whisper'))
  let voskModels = $derived(models.filter((m) => m.engine === 'vosk'))
  let voskOptions = $derived([
    { value: '', label: 'None selected' },
    ...voskModels.map((m) => ({ value: m.id, label: m.label })),
  ])
  let whisperOptions = $derived(whisperModels.map((m) => ({ value: m.id, label: m.label })))

  let unlisten: (() => void) | undefined

  async function refreshModels() {
    try {
      models = await api.listModels()
      modelsError = ''
    } catch (err) {
      modelsError = String(err)
    }
  }

  onMount(async () => {
    await refreshModels()
    try {
      sttConfigured = await api.hasApiKey('stt')
    } catch {
      sttConfigured = false
    }
    api.onModelProgress((p) => {
      progress = { ...progress, [p.id]: { done: p.done, total: p.total } }
    }).then((fn) => {
      unlisten = fn
    })
  })

  onDestroy(() => {
    unlisten?.()
  })

  function progressPercent(id: string): number | null {
    const p = progress[id]
    if (!p || p.total <= 0) return null
    return Math.min(100, Math.round((p.done / p.total) * 100))
  }

  async function install(id: string) {
    busy = { ...busy, [id]: true }
    modelsError = ''
    try {
      await api.downloadModel(id)
      await refreshModels()
    } catch (err) {
      modelsError = `Failed to download "${id}": ${String(err)}`
    } finally {
      busy = { ...busy, [id]: false }
      const rest = { ...progress }
      delete rest[id]
      progress = rest
    }
  }

  async function remove(id: string) {
    busy = { ...busy, [id]: true }
    modelsError = ''
    try {
      await api.removeModel(id)
      await refreshModels()
    } catch (err) {
      modelsError = `Failed to remove "${id}": ${String(err)}`
    } finally {
      busy = { ...busy, [id]: false }
    }
  }

  async function saveSttKey() {
    if (!sttApiKey.trim()) return
    sttKeyError = ''
    try {
      await api.setApiKey('stt', sttApiKey)
    } catch (err) {
      sttKeyError = `Failed to save API key: ${String(err)}`
      return
    }
    sttApiKey = ''
    sttConfigured = true
    sttKeyJustSaved = true
    setTimeout(() => {
      sttKeyJustSaved = false
    }, 2000)
  }
</script>

<Section title="Engines" description="Which speech-to-text engine transcribes your dictation.">
  <Field label="Active engine" for="active-engine">
    <Select
      id="active-engine"
      options={ENGINE_OPTIONS}
      bind:value={
        () => settings.engine.active,
        (v) => settingsStore.patch({ engine: { active: v as EngineKind } })
      }
    />
  </Field>
</Section>

<Section title="Whisper models" description="Runs fully offline. Larger models are more accurate but slower.">
  {#if modelsError}
    <p class="error">{modelsError}</p>
  {/if}
  <Field label="Active model" for="whisper-model">
    <Select
      id="whisper-model"
      options={whisperOptions}
      bind:value={
        () => settings.engine.whisper_model,
        (v) => settingsStore.patch({ engine: { whisper_model: v } })
      }
    />
  </Field>
  <ul class="model-list">
    {#each whisperModels as model (model.id)}
      <li>
        <div class="model-row">
          <div class="model-info">
            <span class="model-label">{model.label}</span>
            <span class="model-size">{model.size_mb} MB</span>
          </div>
          <div class="model-actions">
            {#if model.installed}
              <span class="badge badge-installed">Installed</span>
              <button type="button" onclick={() => remove(model.id)} disabled={busy[model.id]}>
                Remove
              </button>
            {:else}
              <button type="button" onclick={() => install(model.id)} disabled={busy[model.id]}>
                {busy[model.id] ? 'Downloading…' : 'Install'}
              </button>
            {/if}
          </div>
        </div>
        {#if busy[model.id]}
          <div class="progress-track" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={progressPercent(model.id) ?? undefined}>
            <div class="progress-fill" style:width="{progressPercent(model.id) ?? 0}%"></div>
          </div>
        {/if}
      </li>
    {/each}
  </ul>
</Section>

<Section title="Vosk models" description="A lighter-weight offline alternative to Whisper.">
  <Field label="Active model" for="vosk-model">
    <Select
      id="vosk-model"
      options={voskOptions}
      bind:value={
        () => settings.engine.vosk_model ?? '',
        (v) => settingsStore.patch({ engine: { vosk_model: v === '' ? null : v } })
      }
    />
  </Field>
  <ul class="model-list">
    {#each voskModels as model (model.id)}
      <li>
        <div class="model-row">
          <div class="model-info">
            <span class="model-label">{model.label}</span>
            <span class="model-size">{model.size_mb} MB</span>
          </div>
          <div class="model-actions">
            {#if model.installed}
              <span class="badge badge-installed">Installed</span>
              <button type="button" onclick={() => remove(model.id)} disabled={busy[model.id]}>
                Remove
              </button>
            {:else}
              <button type="button" onclick={() => install(model.id)} disabled={busy[model.id]}>
                {busy[model.id] ? 'Downloading…' : 'Install'}
              </button>
            {/if}
          </div>
        </div>
        {#if busy[model.id]}
          <div class="progress-track" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={progressPercent(model.id) ?? undefined}>
            <div class="progress-fill" style:width="{progressPercent(model.id) ?? 0}%"></div>
          </div>
        {/if}
      </li>
    {/each}
  </ul>
</Section>

<Section title="Cloud engine" description="An OpenAI-compatible speech-to-text endpoint.">
  <Field label="Base URL" for="cloud-stt-url">
    <TextInput
      id="cloud-stt-url"
      type="url"
      placeholder="https://api.openai.com/v1"
      bind:value={
        () => settings.engine.cloud.base_url,
        (v) => settingsStore.patch({ engine: { cloud: { base_url: v } } })
      }
    />
  </Field>
  <Field label="Model" for="cloud-stt-model">
    <TextInput
      id="cloud-stt-model"
      placeholder="whisper-1"
      bind:value={
        () => settings.engine.cloud.model,
        (v) => settingsStore.patch({ engine: { cloud: { model: v } } })
      }
    />
  </Field>
  <Field label="API key" for="cloud-stt-key">
    <div class="key-row">
      <TextInput id="cloud-stt-key" type="password" placeholder="sk-…" bind:value={() => sttApiKey, (v) => (sttApiKey = v)} />
      <button type="button" onclick={saveSttKey} disabled={!sttApiKey.trim()}>Save</button>
      {#if sttKeyJustSaved}
        <span class="badge badge-installed">Saved</span>
      {:else if sttConfigured}
        <span class="badge badge-installed">Configured</span>
      {:else}
        <span class="badge badge-missing">Not set</span>
      {/if}
    </div>
    {#if sttKeyError}
      <p class="error">{sttKeyError}</p>
    {/if}
  </Field>
</Section>

<style>
  .error {
    color: var(--danger);
    font-size: 13px;
  }

  .model-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .model-list li {
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
  }

  .model-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .model-info {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .model-label {
    font-size: 13px;
    font-weight: 500;
  }

  .model-size {
    font-size: 12px;
    color: var(--text-muted);
  }

  .model-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  button {
    padding: 5px var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    cursor: pointer;
    font-size: 13px;
  }

  button:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .badge {
    font-size: 11px;
    font-weight: 600;
    padding: 2px var(--space-2);
    border-radius: 999px;
  }

  .badge-installed {
    background: var(--success);
    color: var(--accent-contrast);
  }

  .badge-missing {
    background: var(--bg-sunken);
    color: var(--text-muted);
  }

  .progress-track {
    margin-top: var(--space-2);
    height: 6px;
    border-radius: 999px;
    background: var(--bg-sunken);
    overflow: hidden;
  }

  .progress-fill {
    height: 100%;
    background: var(--accent);
    transition: width 150ms ease;
  }

  .key-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
</style>
