<script lang="ts">
  import Section from '../lib/components/Section.svelte'
  import Field from '../lib/components/Field.svelte'
  import Select from '../lib/components/Select.svelte'
  import Toggle from '../lib/components/Toggle.svelte'
  import TextInput from '../lib/components/TextInput.svelte'
  import { settingsStore } from '../lib/stores'
  import type { DictationMode } from '../lib/types'

  let settings = $derived($settingsStore!)

  const MODE_OPTIONS: { value: DictationMode; label: string }[] = [
    { value: 'push_to_talk', label: 'Push to talk (hold hotkey)' },
    { value: 'toggle', label: 'Toggle (press to start, press to stop)' },
  ]

  let timeoutEnabled = $derived(settings.dictation.silence_timeout_secs !== null)
  let timeoutValue = $derived(String(settings.dictation.silence_timeout_secs ?? 30))

  function setTimeoutEnabled(enabled: boolean) {
    settingsStore.patch({
      dictation: { silence_timeout_secs: enabled ? Number(timeoutValue) || 30 : null },
    })
  }

  function setTimeoutValue(raw: string) {
    const n = Math.max(1, Math.round(Number(raw) || 0))
    settingsStore.patch({ dictation: { silence_timeout_secs: n } })
  }
</script>

<Section title="Dictation" description="How recording starts, stops, and is triggered.">
  <Field label="Mode" for="mode">
    <Select
      id="mode"
      options={MODE_OPTIONS}
      bind:value={
        () => settings.dictation.mode,
        (v) => settingsStore.patch({ dictation: { mode: v as DictationMode } })
      }
    />
  </Field>

  <Field label="Silence timeout" hint="Automatically stop recording after a period of silence.">
    <div class="timeout-row">
      <Toggle
        id="silence-timeout-enabled"
        bind:checked={() => timeoutEnabled, setTimeoutEnabled}
      />
      <span class="timeout-label">{timeoutEnabled ? 'On' : 'Off'}</span>
      {#if timeoutEnabled}
        <TextInput
          type="number"
          min={1}
          max={600}
          bind:value={() => timeoutValue, setTimeoutValue}
        />
        <span class="unit">seconds</span>
      {/if}
    </div>
  </Field>

  <Field label="Show HUD" for="hud" hint="A small floating indicator while dictation is active.">
    <Toggle
      id="hud"
      bind:checked={
        () => settings.dictation.hud,
        (v) => settingsStore.patch({ dictation: { hud: v } })
      }
    />
  </Field>
</Section>

<style>
  .timeout-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .timeout-label {
    font-size: 13px;
    color: var(--text-muted);
    min-width: 2.5ch;
  }

  .unit {
    font-size: 13px;
    color: var(--text-muted);
  }
</style>
