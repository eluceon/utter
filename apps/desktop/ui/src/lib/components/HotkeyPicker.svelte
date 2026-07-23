<script lang="ts">
  // Captures a keydown/keyup gesture and normalizes it into the chord string
  // format `utter_inject::hotkey::parse_hotkey` accepts: `+`-separated
  // tokens, each one of `ctrl`/`alt`/`shift`/`super` (the modifier names,
  // canonicalized — the Rust parser also accepts `control` and `meta`/`win`
  // as aliases for `ctrl`/`super`, but this picker only ever emits the
  // canonical short forms) or a single letter/digit/`f1`..`f24` base key. A
  // chord made entirely of modifiers (e.g. the default `ctrl+super`) is
  // valid and accepted here too.

  interface Props {
    value?: string
    id?: string
    disabled?: boolean
  }

  let { value = $bindable(''), id, disabled = false }: Props = $props()

  const MODIFIER_ORDER = ['ctrl', 'alt', 'shift', 'super'] as const
  type ModifierToken = (typeof MODIFIER_ORDER)[number]

  const MODIFIER_KEY_NAMES: Record<string, ModifierToken> = {
    Control: 'ctrl',
    Alt: 'alt',
    Shift: 'shift',
    Meta: 'super',
  }

  let capturing = $state(false)
  let preview = $state('')
  let hint = $state('')

  /** Keys physically down right now, for detecting "every key released". */
  let down = new Set<string>()
  /** Every token seen during this gesture — the chord is finalized from
   * this set once `down` empties out, so releasing modifiers before the
   * base key (or vice versa) still captures the full combo. */
  let combo = new Set<string>()

  function tokenFor(key: string): string | null {
    if (key in MODIFIER_KEY_NAMES) return MODIFIER_KEY_NAMES[key]
    if (/^F(?:[1-9]|1[0-9]|2[0-4])$/.test(key)) return key.toLowerCase()
    if (/^[a-zA-Z0-9]$/.test(key)) return key.toLowerCase()
    return null
  }

  function isModifier(token: string): token is ModifierToken {
    return (MODIFIER_ORDER as readonly string[]).includes(token)
  }

  function formatCombo(tokens: Set<string>): string {
    const mods = MODIFIER_ORDER.filter((m) => tokens.has(m))
    const rest = [...tokens].filter((t) => !isModifier(t))
    return [...mods, ...rest].join('+')
  }

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

    const token = tokenFor(event.key)
    if (!token) return

    const hasBaseKey = [...combo].some((t) => !isModifier(t))
    if (!isModifier(token) && hasBaseKey && !combo.has(token)) {
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

    const token = tokenFor(event.key)
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
