<script lang="ts">
  // Captures a keydown/keyup gesture and normalizes it into a hotkey chord
  // string. The token-normalization rules live in `../hotkey` (unit-tested
  // there, including the shift+digit case); this component only owns the
  // capture gesture (which keys are currently down, when to finalize).

  import { formatCombo, isModifierToken, tokenFor } from '../hotkey'

  interface Props {
    value?: string
    id?: string
    disabled?: boolean
  }

  let { value = $bindable(''), id, disabled = false }: Props = $props()

  let capturing = $state(false)
  let preview = $state('')
  let hint = $state('')

  /** Keys physically down right now, for detecting "every key released". */
  let down = new Set<string>()
  /** Every token seen during this gesture — the chord is finalized from
   * this set once `down` empties out, so releasing modifiers before the
   * base key (or vice versa) still captures the full combo. */
  let combo = new Set<string>()

  function start() {
    if (disabled) return
    capturing = true
    preview = ''
    hint = 'Press keys… (Esc to cancel)'
    down = new Set()
    combo = new Set()
  }

  function stop(commit: boolean) {
    if (commit && combo.size > 0) {
      value = formatCombo(combo)
    }
    capturing = false
    preview = ''
    hint = ''
    down = new Set()
    combo = new Set()
  }

  function onKeydown(event: KeyboardEvent) {
    if (!capturing) return
    event.preventDefault()

    if (event.key === 'Escape') {
      stop(false)
      return
    }

    const token = tokenFor(event.code, event.key)
    if (!token) return

    const hasBaseKey = [...combo].some((t) => !isModifierToken(t))
    if (!isModifierToken(token) && hasBaseKey && !combo.has(token)) {
      hint = 'A hotkey may only have one base key'
      return
    }

    down.add(token)
    combo.add(token)
    preview = formatCombo(combo)
    hint = 'Release all keys to confirm'
  }

  function onKeyup(event: KeyboardEvent) {
    if (!capturing) return
    event.preventDefault()

    const token = tokenFor(event.code, event.key)
    if (token) down.delete(token)

    if (down.size === 0) {
      stop(true)
    }
  }

  function onBlur() {
    // Losing focus mid-capture (e.g. Alt-Tab) must not leave the picker
    // stuck listening forever.
    if (capturing) stop(false)
  }
</script>

<div class="hotkey-picker">
  <button
    type="button"
    {id}
    {disabled}
    class="capture-button"
    class:capturing
    onclick={start}
    onkeydown={onKeydown}
    onkeyup={onKeyup}
    onblur={onBlur}
  >
    {#if capturing}
      {preview || 'Press keys…'}
    {:else}
      {value || 'Click to set…'}
    {/if}
  </button>
  {#if hint}
    <span class="hint">{hint}</span>
  {/if}
</div>

<style>
  .hotkey-picker {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    width: 100%;
    max-width: 320px;
  }

  .capture-button {
    width: 100%;
    padding: 6px var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--text);
    font-size: 13px;
    font-family: var(--font-mono);
    height: 32px;
    text-align: left;
    cursor: pointer;
  }

  .capture-button:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .capture-button.capturing {
    border-color: var(--accent);
    background: var(--bg-sunken);
  }

  .hint {
    font-size: 12px;
    color: var(--text-muted);
  }
</style>
