<script module lang="ts">
  export interface SelectOption {
    value: string
    label: string
  }
</script>

<script lang="ts">
  interface Props {
    value?: string
    options: SelectOption[]
    id?: string
    disabled?: boolean
  }

  let { value = $bindable(''), options, id, disabled = false }: Props = $props()
</script>

<select bind:value {id} {disabled} class="select">
  {#each options as option (option.value)}
    <option value={option.value}>{option.label}</option>
  {/each}
</select>

<style>
  .select {
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

  .select:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
</style>
