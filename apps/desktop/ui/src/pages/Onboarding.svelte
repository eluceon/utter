<script lang="ts">
  import { onDestroy, onMount } from 'svelte'

  import HotkeyPicker from '../lib/components/HotkeyPicker.svelte'
  import * as api from '../lib/api'
  import { settingsStore } from '../lib/stores'
  import type { ModelInfo, PermissionReport } from '../lib/types'

  interface Props {
    onDone: () => void
  }

  let { onDone }: Props = $props()

  let settings = $derived($settingsStore!)

  const STEPS = ['Welcome', 'Microphone', 'Model', 'Hotkey', 'Permissions', 'Done'] as const
  let step = $state(0)

  function next() {
    if (step < STEPS.length - 1) step += 1
  }
  function back() {
    if (step > 0) step -= 1
  }

  // --- Step: microphone ---
  let devices = $state<string[]>([])
  let devicesError = $state('')
  let devicesChecked = $state(false)

  async function checkMic() {
    try {
      devices = await api.listDevices()
    } catch (err) {
      devicesError = String(err)
    } finally {
      devicesChecked = true
    }
  }

  // --- Step: model ---
  let models = $state<ModelInfo[]>([])
  let modelsError = $state('')
  let progress = $state<Record<string, { done: number; total: number }>>({})
  let busy = $state<Record<string, boolean>>({})
  let unlistenProgress: (() => void) | undefined

  let whisperModels = $derived(models.filter((m) => m.engine === 'whisper'))
  let anyInstalled = $derived(models.some((m) => m.installed))

  async function refreshModels() {
    try {
      models = await api.listModels()
      modelsError = ''
    } catch (err) {
      modelsError = String(err)
    }
  }

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

  // --- Step: permissions ---
  let permissions = $state<PermissionReport | null>(null)
  let permissionsError = $state('')
  let fixCopied = $state(false)

  async function checkPermissions() {
    try {
      permissions = await api.permissionsReport()
      permissionsError = ''
    } catch (err) {
      permissionsError = String(err)
    }
  }

  async function copyFixCommand() {
    if (!permissions) return
    try {
      await navigator.clipboard.writeText(permissions.fix_command)
      fixCopied = true
      setTimeout(() => {
        fixCopied = false
      }, 1500)
    } catch {
      // Best-effort; clipboard access may be denied.
    }
  }

  onMount(() => {
    void refreshModels()
    api.onModelProgress((p) => {
      progress = { ...progress, [p.id]: { done: p.done, total: p.total } }
    }).then((fn) => {
      unlistenProgress = fn
    })
  })

  onDestroy(() => {
    unlistenProgress?.()
  })

  $effect(() => {
    if (step === 1 && !devicesChecked) void checkMic()
    if (step === 4 && !permissions && !permissionsError) void checkPermissions()
  })
</script>

<div class="onboarding">
  <div class="card">
    <div class="steps">
      {#each STEPS as label, i (label)}
        <span class="step-dot" class:active={i === step} class:done={i < step}>{i + 1}</span>
      {/each}
    </div>

    {#if step === 0}
      <h1>Welcome to Utter</h1>
      <p>A quick, skippable setup: microphone, a speech model, your hotkey, and permissions.</p>
    {:else if step === 1}
      <h1>Microphone</h1>
      <p class="muted">
        This only confirms your system reports an input device — it can't test live recording
        from this wizard.
      </p>
      {#if devicesError}
        <p class="error">{devicesError}</p>
      {:else if devicesChecked}
        {#if devices.length === 0}
          <p class="warn">No input devices were found. Check your microphone connection.</p>
        {:else}
          <p>Found {devices.length} input device{devices.length === 1 ? '' : 's'}:</p>
          <ul>
            {#each devices as device (device)}
              <li>{device}</li>
            {/each}
          </ul>
        {/if}
      {:else}
        <p class="muted">Checking…</p>
      {/if}
    {:else if step === 2}
      <h1>Speech model</h1>
      <p class="muted">Install at least one offline Whisper model to get started (you can add more later).</p>
      {#if modelsError}
        <p class="error">{modelsError}</p>
      {/if}
      <ul class="model-list">
        {#each whisperModels as model (model.id)}
          <li>
            <div class="model-row">
              <div class="model-info">
                <span class="model-label">{model.label}</span>
                <span class="model-size">{model.size_mb} MB</span>
              </div>
              {#if model.installed}
                <span class="badge">Installed</span>
              {:else}
                <button type="button" onclick={() => install(model.id)} disabled={busy[model.id]}>
                  {busy[model.id] ? 'Downloading…' : 'Install'}
                </button>
              {/if}
            </div>
            {#if busy[model.id]}
              <div class="progress-track">
                <div class="progress-fill" style:width="{progressPercent(model.id) ?? 0}%"></div>
              </div>
            {/if}
          </li>
        {/each}
      </ul>
      {#if anyInstalled}
        <p class="ok">A model is installed — you're ready to dictate.</p>
      {/if}
    {:else if step === 3}
      <h1>Hotkey</h1>
      <p class="muted">Pick the key combination that starts/stops dictation.</p>
      <HotkeyPicker
        bind:value={
          () => settings.profiles[0].hotkey,
          (v) =>
            settingsStore.patch({
              profiles: settings.profiles.map((p, i) => (i === 0 ? { ...p, hotkey: v } : p)),
            })
        }
      />
    {:else if step === 4}
      <h1>Permissions</h1>
      <p class="muted">Linux hotkeys and text injection need two OS-level permissions.</p>
      {#if permissionsError}
        <p class="error">{permissionsError}</p>
      {:else if permissions}
        <ul class="perm-list">
          <li>
            <span class="perm-status" class:ok={permissions.input_group}>
              {permissions.input_group ? '✓' : '✗'}
            </span>
            Input device group membership
          </li>
          <li>
            <span class="perm-status" class:ok={permissions.uinput_writable}>
              {permissions.uinput_writable ? '✓' : '✗'}
            </span>
            /dev/uinput writable
          </li>
        </ul>
        {#if !permissions.input_group || !permissions.uinput_writable}
          <pre class="fix-command">{permissions.fix_command}</pre>
          <button type="button" onclick={copyFixCommand}>{fixCopied ? 'Copied' : 'Copy fix command'}</button>
        {:else}
          <p class="ok">All required permissions are already granted.</p>
        {/if}
      {:else}
        <p class="muted">Checking…</p>
      {/if}
    {:else if step === 5}
      <h1>You're all set</h1>
      <p>You can revisit any of this later from the settings sidebar.</p>
    {/if}

    <div class="actions">
      {#if step > 0}
        <button type="button" onclick={back}>Back</button>
      {/if}
      <div class="spacer"></div>
      {#if step < STEPS.length - 1}
        <button type="button" class="ghost" onclick={onDone}>Skip setup</button>
        <button type="button" class="primary" onclick={next}>Continue</button>
      {:else}
        <button type="button" class="primary" onclick={onDone}>Finish</button>
      {/if}
    </div>
  </div>
</div>

<style>
  .onboarding {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100vh;
    background: var(--bg);
  }

  .card {
    width: 100%;
    max-width: 480px;
    padding: var(--space-6);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-elevated);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .steps {
    display: flex;
    gap: var(--space-2);
    margin-bottom: var(--space-2);
  }

  .step-dot {
    width: 22px;
    height: 22px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 11px;
    background: var(--bg-sunken);
    color: var(--text-muted);
  }

  .step-dot.active {
    background: var(--accent);
    color: var(--accent-contrast);
  }

  .step-dot.done {
    background: var(--success);
    color: var(--accent-contrast);
  }

  h1 {
    font-size: 18px;
    font-weight: 700;
  }

  .muted {
    color: var(--text-muted);
    font-size: 13px;
  }

  .error {
    color: var(--danger);
    font-size: 13px;
  }

  .warn {
    color: var(--warning-text);
    background: var(--warning-bg);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    font-size: 13px;
  }

  .ok {
    color: var(--success);
    font-size: 13px;
  }

  ul {
    margin: 0;
    padding-left: var(--space-4);
    font-size: 13px;
  }

  .model-list {
    list-style: none;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .model-list li {
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }

  .model-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .model-info {
    display: flex;
    flex-direction: column;
  }

  .model-label {
    font-size: 13px;
    font-weight: 500;
  }

  .model-size {
    font-size: 12px;
    color: var(--text-muted);
  }

  .badge {
    font-size: 11px;
    font-weight: 600;
    padding: 2px var(--space-2);
    border-radius: 999px;
    background: var(--success);
    color: var(--accent-contrast);
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

  .perm-list {
    list-style: none;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .perm-status {
    display: inline-block;
    width: 1.5em;
    color: var(--danger);
    font-weight: 700;
  }

  .perm-status.ok {
    color: var(--success);
  }

  .fix-command {
    background: var(--bg-sunken);
    padding: var(--space-2);
    border-radius: var(--radius-sm);
    font-size: 12px;
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin-top: var(--space-3);
  }

  .spacer {
    flex: 1;
  }

  button {
    padding: 6px var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
    cursor: pointer;
    font-size: 13px;
  }

  button:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  button.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--accent-contrast);
    font-weight: 600;
  }

  button.primary:not(:disabled):hover {
    background: var(--accent-hover);
    border-color: var(--accent-hover);
  }

  button.ghost {
    background: none;
    border-color: transparent;
    color: var(--text-muted);
  }

  button.ghost:not(:disabled):hover {
    background: var(--surface-hover);
  }
</style>
