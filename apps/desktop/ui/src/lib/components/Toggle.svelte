<script lang="ts">
  interface Props {
    checked?: boolean
    id?: string
    disabled?: boolean
    label?: string
  }

  let { checked = $bindable(false), id, disabled = false, label }: Props = $props()
</script>

<label class="toggle" class:disabled>
  <input type="checkbox" role="switch" bind:checked {id} {disabled} aria-checked={checked} />
  <span class="track" aria-hidden="true"><span class="thumb"></span></span>
  {#if label}
    <span class="text">{label}</span>
  {/if}
</label>

<style>
  .toggle {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    cursor: pointer;
    user-select: none;
  }

  .toggle.disabled {
    cursor: not-allowed;
    opacity: 0.55;
  }

  input {
    /* Visually hidden but still focusable, clickable, and tabbable — a real
       checkbox, not a `div` with a click handler. */
    position: absolute;
    width: 1px;
    height: 1px;
    margin: -1px;
    padding: 0;
    border: 0;
    clip: rect(0 0 0 0);
    overflow: hidden;
    white-space: nowrap;
  }

  .track {
    width: 36px;
    height: 20px;
    border-radius: 999px;
    background: var(--border);
    display: inline-flex;
    align-items: center;
    padding: 2px;
    flex-shrink: 0;
    transition: background-color 120ms ease;
  }

  .thumb {
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--bg);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.25);
    transition: transform 120ms ease;
  }

  input:checked + .track {
    background: var(--accent);
  }

  input:checked + .track .thumb {
    transform: translateX(16px);
  }

  input:focus-visible + .track {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .text {
    font-size: 13px;
  }
</style>
