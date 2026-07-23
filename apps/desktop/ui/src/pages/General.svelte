<script lang="ts">
  import Section from '../lib/components/Section.svelte'
  import Field from '../lib/components/Field.svelte'
  import Select from '../lib/components/Select.svelte'
  import Toggle from '../lib/components/Toggle.svelte'
  import { settingsStore } from '../lib/stores'
  import type { Theme } from '../lib/types'

  // App.svelte only mounts pages once `$settingsStore` has finished loading,
  // so this non-null assertion is safe for the component's whole lifetime.
  let settings = $derived($settingsStore!)

  const LANGUAGE_OPTIONS = [
    { value: '', label: 'Auto-detect' },
    { value: 'en', label: 'English' },
    { value: 'es', label: 'Spanish' },
    { value: 'fr', label: 'French' },
    { value: 'de', label: 'German' },
    { value: 'ru', label: 'Russian' },
    { value: 'pt', label: 'Portuguese' },
    { value: 'it', label: 'Italian' },
    { value: 'zh', label: 'Chinese' },
    { value: 'ja', label: 'Japanese' },
  ]

  const THEME_OPTIONS: { value: Theme; label: string }[] = [
    { value: 'system', label: 'Match system' },
    { value: 'light', label: 'Light' },
    { value: 'dark', label: 'Dark' },
  ]
</script>

<Section title="General" description="Language, appearance, and startup behavior.">
  <Field label="Language" for="language" hint="Used as a hint for the speech-to-text engine.">
    <Select
      id="language"
      options={LANGUAGE_OPTIONS}
      bind:value={
        () => settings.general.language ?? '',
        (v) => settingsStore.patch({ general: { language: v === '' ? null : v } })
      }
    />
  </Field>

  <Field label="Theme" for="theme">
    <Select
      id="theme"
      options={THEME_OPTIONS}
      bind:value={
        () => settings.general.theme,
        (v) => settingsStore.patch({ general: { theme: v as Theme } })
      }
    />
  </Field>

  <Field label="Launch at login" for="autostart">
    <Toggle
      id="autostart"
      bind:checked={
        () => settings.general.autostart,
        (v) => settingsStore.patch({ general: { autostart: v } })
      }
    />
  </Field>
</Section>

<style>
  /* This page composes entirely from Section/Field/Select/Toggle, each of
     which is already fully styled — nothing page-specific to add here. */
</style>
