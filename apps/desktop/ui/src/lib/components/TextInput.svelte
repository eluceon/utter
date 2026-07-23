<script lang="ts">
  interface Props {
    value?: string
    id?: string
    type?: 'text' | 'password' | 'number' | 'url'
    placeholder?: string
    disabled?: boolean
    min?: number
    max?: number
    step?: number
  }

  let {
    value = $bindable(''),
    id,
    type = 'text',
    placeholder,
    disabled = false,
    min,
    max,
    step,
  }: Props = $props()
</script>

<input
  class="text-input"
  {type}
  {id}
  {placeholder}
  {disabled}
  {min}
  {max}
  {step}
  bind:value
  autocomplete="off"
  spellcheck="false"
/>

<style>
  .text-input {
    width: 100%;
    max-width: 320px;
    padding: 6px var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--text);
    font-size: 13px;
    height: 32px;
  }

  .text-input:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .text-input::placeholder {
    color: var(--text-muted);
  }
</style>
