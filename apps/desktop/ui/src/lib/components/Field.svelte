<script lang="ts">
  import type { Snippet } from 'svelte'

  interface Props {
    label: string
    hint?: string
    /** Matches the `id` of the control rendered inside, so the label is
     * properly associated for screen readers and "click label to focus". */
    for?: string
    children?: Snippet
  }

  let { label, hint, for: forId, children }: Props = $props()
</script>

<div class="field">
  <div class="label-col">
    <label for={forId}>{label}</label>
    {#if hint}
      <span class="hint">{hint}</span>
    {/if}
  </div>
  <div class="control-col">
    {#if children}
      {@render children()}
    {/if}
  </div>
</div>

<style>
  .field {
    display: grid;
    grid-template-columns: minmax(140px, 220px) 1fr;
    align-items: start;
    gap: var(--space-4);
  }

  .label-col {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    padding-top: 6px;
  }

  label {
    font-size: 13px;
    font-weight: 500;
  }

  .hint {
    font-size: 12px;
    color: var(--text-muted);
    max-width: 32ch;
  }

  .control-col {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-1);
    min-width: 0;
  }

  @media (max-width: 640px) {
    .field {
      grid-template-columns: 1fr;
    }
  }
</style>
