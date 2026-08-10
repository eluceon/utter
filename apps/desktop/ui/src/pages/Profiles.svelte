<script lang="ts">
  import { onMount } from 'svelte'

  import Section from '../lib/components/Section.svelte'
  import Field from '../lib/components/Field.svelte'
  import Select from '../lib/components/Select.svelte'
  import TextInput from '../lib/components/TextInput.svelte'
  import Toggle from '../lib/components/Toggle.svelte'
  import HotkeyPicker from '../lib/components/HotkeyPicker.svelte'
  import * as api from '../lib/api'
  import { chordsConflict, parseChordTokens } from '../lib/hotkey'
  import { previewModelOptions } from '../lib/models'
  import { mergeDeep, settingsStore, type DeepPartial } from '../lib/stores'
  import type { EngineKind, LanguageProfile, ModelInfo, Tone } from '../lib/types'

  // App.svelte only mounts pages once `$settingsStore` has finished loading,
  // so this non-null assertion is safe for the component's whole lifetime.
  let settings = $derived($settingsStore!)

  const ENGINE_OPTIONS: { value: EngineKind; label: string }[] = [
    { value: 'whisper', label: 'Whisper (local)' },
    { value: 'sherpa', label: 'Sherpa-onnx (local)' },
    { value: 'cloud', label: 'Cloud (OpenAI-compatible)' },
  ]

  const TONE_OPTIONS: { value: Tone; label: string }[] = [
    { value: 'verbatim', label: 'Verbatim (no changes)' },
    { value: 'clean', label: 'Clean (punctuation, casing)' },
    { value: 'formal', label: 'Formal' },
    { value: 'notes', label: 'Notes (terse, bulleted)' },
    { value: 'code_comment', label: 'Code comment' },
  ]

  let models = $state<ModelInfo[]>([])
  let modelsError = $state('')

  let whisperOptions = $derived(
    models.filter((m) => m.engine === 'whisper').map((m) => ({ value: m.id, label: m.label })),
  )
  let sherpaOptions = $derived([
    { value: '', label: 'None selected' },
    ...models.filter((m) => m.engine === 'sherpa').map((m) => ({ value: m.id, label: m.label })),
  ])
  // Streaming models only, never the engine models above — see
  // `previewModelOptions`, which is where that separation is pinned.
  let previewOptions = $derived(previewModelOptions(models))

  onMount(async () => {
    try {
      models = await api.listModels()
    } catch (err) {
      modelsError = String(err)
    }
  })

  // Every profile's hotkey, parsed into the token set `chordsConflict`
  // compares — `null` for a chord that would fail to parse on the Rust side
  // too (see `parseChordTokens`), so it takes part in no conflict here
  // either, mirroring `parse_profile_hotkeys` dropping such a profile from
  // hotkey registration instead of reporting a conflict for it.
  let parsedHotkeys = $derived(settings.profiles.map((p) => parseChordTokens(p.hotkey)))

  // Maps a profile's index to the ids of every other profile whose chord
  // conflicts with it, using `chordsConflict` — a deliberately close mirror
  // of `utter_inject::hotkey::find_conflicts`'s own pairwise scan, so the two
  // stay easy to compare if Rust's rule ever changes. See `../lib/hotkey.ts`
  // for why the two are expected to agree rather than reimplementing the
  // same idea independently.
  let conflictsByIndex = $derived.by(() => {
    const map = new Map<number, string[]>()
    for (let i = 0; i < parsedHotkeys.length; i++) {
      for (let j = i + 1; j < parsedHotkeys.length; j++) {
        const a = parsedHotkeys[i]
        const b = parsedHotkeys[j]
        if (!a || !b || !chordsConflict(a, b)) continue
        map.set(i, [...(map.get(i) ?? []), settings.profiles[j].id])
        map.set(j, [...(map.get(j) ?? []), settings.profiles[i].id])
      }
    }
    return map
  })

  function updateProfile(index: number, changes: DeepPartial<LanguageProfile>) {
    const profiles = settings.profiles.map((profile, i) =>
      i === index ? mergeDeep(profile, changes) : profile,
    )
    settingsStore.patch({ profiles })
  }

  function nextProfileId(): string {
    const existing = new Set(settings.profiles.map((p) => p.id))
    let n = settings.profiles.length + 1
    while (existing.has(`profile-${n}`)) n += 1
    return `profile-${n}`
  }

  function addProfile() {
    const newProfile: LanguageProfile = {
      id: nextProfileId(),
      hotkey: '',
      language: '',
      engine: {
        active: 'sherpa',
        whisper_model: 'small',
        sherpa_model: null,
        cloud: { base_url: 'https://api.openai.com/v1', model: 'whisper-1' },
      },
      draft: null,
      refine: { enabled: false, tone: 'clean' },
    }
    settingsStore.patch({ profiles: [...settings.profiles, newProfile] })
  }

  // `ProfileRegistry::new` (crates/apps/desktop/src-tauri/src/profiles.rs)
  // only *warns* that dictation has no hotkey once the profile list is
  // empty — it does not refuse the state. A hand-edited `profiles = []` is
  // the only way that happens today (`predates_profiles` only triggers
  // migration when the `profiles` key is absent, not when it's empty), and
  // this UI has no such loophole: disabling Remove below the last profile
  // means it can never construct that state in the first place, rather than
  // constructing it and then explaining the warning afterwards.
  function removeProfile(index: number) {
    if (settings.profiles.length <= 1) return
    settingsStore.patch({ profiles: settings.profiles.filter((_, i) => i !== index) })
  }
</script>

{#if modelsError}
  <p class="error">{modelsError}</p>
{/if}

{#each settings.profiles as profile, index (index)}
  <Section
    title={profile.id || `Profile ${index + 1}`}
    description="A hotkey, an engine and model, and a refinement policy for one language."
  >
    <Field label="ID" for="profile-{index}-id" hint="Used in config, history, and the HUD.">
      <TextInput
        id="profile-{index}-id"
        bind:value={
          () => profile.id,
          (v) => updateProfile(index, { id: v })
        }
      />
    </Field>

    <Field
      label="Language"
      for="profile-{index}-language"
      hint="BCP-47 tag passed to the engine as a transcription hint, e.g. en, ru."
    >
      <TextInput
        id="profile-{index}-language"
        bind:value={
          () => profile.language,
          (v) => updateProfile(index, { language: v })
        }
      />
    </Field>

    <Field
      label="Hotkey"
      for="profile-{index}-hotkey"
      hint="Modifiers plus one key, e.g. ctrl+alt+d, or modifiers alone."
    >
      <HotkeyPicker
        id="profile-{index}-hotkey"
        bind:value={
          () => profile.hotkey,
          (v) => updateProfile(index, { hotkey: v })
        }
      />
      {#if conflictsByIndex.has(index)}
        <p class="warning">
          Conflicts with {conflictsByIndex.get(index)?.join(', ')} — one key press could fire
          either.
        </p>
      {/if}
    </Field>

    <Field label="Engine" for="profile-{index}-engine">
      <Select
        id="profile-{index}-engine"
        options={ENGINE_OPTIONS}
        bind:value={
          () => profile.engine.active,
          (v) => updateProfile(index, { engine: { active: v as EngineKind } })
        }
      />
    </Field>

    {#if profile.engine.active === 'whisper'}
      <Field label="Whisper model" for="profile-{index}-model">
        <Select
          id="profile-{index}-model"
          options={whisperOptions}
          bind:value={
            () => profile.engine.whisper_model,
            (v) => updateProfile(index, { engine: { whisper_model: v } })
          }
        />
      </Field>
    {:else if profile.engine.active === 'sherpa'}
      <Field label="Sherpa model" for="profile-{index}-model">
        <Select
          id="profile-{index}-model"
          options={sherpaOptions}
          bind:value={
            () => profile.engine.sherpa_model ?? '',
            (v) => updateProfile(index, { engine: { sherpa_model: v === '' ? null : v } })
          }
        />
      </Field>
    {:else}
      <Field label="Cloud base URL" for="profile-{index}-cloud-url">
        <TextInput
          id="profile-{index}-cloud-url"
          type="url"
          bind:value={
            () => profile.engine.cloud.base_url,
            (v) => updateProfile(index, { engine: { cloud: { base_url: v } } })
          }
        />
      </Field>
      <Field label="Cloud model" for="profile-{index}-cloud-model">
        <TextInput
          id="profile-{index}-cloud-model"
          bind:value={
            () => profile.engine.cloud.model,
            (v) => updateProfile(index, { engine: { cloud: { model: v } } })
          }
        />
      </Field>
    {/if}

    <Field
      label="Live preview"
      for="profile-{index}-preview"
      hint="A streaming model that shows words in the HUD while you speak. The inserted text always comes from the engine above, never from this. Off by default; download preview models on the Engines page."
    >
      <Select
        id="profile-{index}-preview"
        options={previewOptions}
        bind:value={
          () => profile.draft?.model ?? '',
          (v) => updateProfile(index, { draft: v === '' ? null : { model: v } })
        }
      />
    </Field>

    <Field
      label="Refine transcripts"
      for="profile-{index}-refine"
      hint="Also needs the master switch on the Refinement page."
    >
      <Toggle
        id="profile-{index}-refine"
        bind:checked={
          () => profile.refine.enabled,
          (v) => updateProfile(index, { refine: { enabled: v } })
        }
      />
    </Field>

    {#if profile.refine.enabled}
      <Field label="Tone" for="profile-{index}-tone">
        <Select
          id="profile-{index}-tone"
          options={TONE_OPTIONS}
          bind:value={
            () => profile.refine.tone,
            (v) => updateProfile(index, { refine: { tone: v as Tone } })
          }
        />
      </Field>
    {/if}

    <div class="profile-actions">
      <button
        type="button"
        class="remove"
        onclick={() => removeProfile(index)}
        disabled={settings.profiles.length <= 1}
        title={settings.profiles.length <= 1 ? 'At least one profile is required' : undefined}
      >
        Remove profile
      </button>
    </div>
  </Section>
{/each}

<Section title="Add a profile" description="Bind another hotkey to a language, engine, and refinement policy.">
  <div class="profile-actions">
    <button type="button" onclick={addProfile}>Add profile</button>
  </div>
</Section>

<style>
  .error {
    color: var(--danger);
    font-size: 13px;
  }

  .warning {
    color: var(--warning-text);
    background: var(--warning-bg);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    font-size: 12px;
  }

  .profile-actions {
    display: flex;
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

  button.remove {
    color: var(--danger);
    border-color: var(--danger);
    background: var(--bg);
  }

  button.remove:disabled {
    color: var(--text-muted);
    border-color: var(--border);
    background: var(--bg-elevated);
  }
</style>
