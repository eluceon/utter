<script lang="ts">
  import { onMount } from 'svelte'

  import Section from '../lib/components/Section.svelte'
  import Field from '../lib/components/Field.svelte'
  import Select from '../lib/components/Select.svelte'
  import Slider from '../lib/components/Slider.svelte'
  import * as api from '../lib/api'
  import { settingsStore } from '../lib/stores'
  import type { InjectionPreference } from '../lib/types'

  let settings = $derived($settingsStore!)

  const INJECTION_OPTIONS: { value: InjectionPreference; label: string }[] = [
    { value: 'auto', label: 'Auto (best available)' },
    { value: 'clipboard_paste', label: 'Clipboard + paste' },
    { value: 'type', label: 'Simulated typing' },
    { value: 'clipboard_only', label: 'Clipboard only (no auto-paste)' },
  ]

  const LOG_LEVEL_OPTIONS = ['trace', 'debug', 'info', 'warn', 'error'].map((v) => ({
    value: v,
    label: v,
  }))

  let devices = $state<string[]>([])
  let devicesError = $state('')

  onMount(async () => {
    try {
      devices = await api.listDevices()
    } catch (err) {
      devicesError = String(err)
    }
  })

  let deviceOptions = $derived([
    { value: '', label: 'System default' },
    ...devices.map((d) => ({ value: d, label: d })),
  ])
</script>

<Section title="Advanced" description="Text injection, audio input, and diagnostics.">
  <Field label="Text injection" for="injection" hint="How refined text is delivered to the focused app.">
    <Select
      id="injection"
      options={INJECTION_OPTIONS}
      bind:value={
        () => settings.advanced.injection,
        (v) => settingsStore.patch({ advanced: { injection: v as InjectionPreference } })
      }
    />
  </Field>

  <Field label="Audio input device" for="audio-device">
    {#if devicesError}
      <p class="error">{devicesError}</p>
    {/if}
    <Select
      id="audio-device"
      options={deviceOptions}
      bind:value={
        () => settings.advanced.audio_device ?? '',
        (v) => settingsStore.patch({ advanced: { audio_device: v === '' ? null : v } })
      }
    />
  </Field>

  <Field
    label="Voice activity sensitivity"
    for="vad"
    hint="Higher values trigger silence detection more eagerly."
  >
    <Slider
      id="vad"
      min={0}
      max={1}
      step={0.05}
      bind:value={
        () => settings.advanced.vad_sensitivity,
        (v) => settingsStore.patch({ advanced: { vad_sensitivity: v } })
      }
    />
  </Field>

  <Field label="Log level" for="log-level">
    <Select
      id="log-level"
      options={LOG_LEVEL_OPTIONS}
      bind:value={
        () => settings.advanced.log_level,
        (v) => settingsStore.patch({ advanced: { log_level: v } })
      }
    />
  </Field>
</Section>

<style>
  .error {
    color: var(--danger);
    font-size: 13px;
  }
</style>
