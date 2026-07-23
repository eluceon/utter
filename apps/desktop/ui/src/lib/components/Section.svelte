<script lang="ts">
  import type { Snippet } from 'svelte'

  interface Props {
    title: string
    description?: string
    children?: Snippet
  }

  let { title, description, children }: Props = $props()
</script>

<section class="section">
  <header>
    <h2>{title}</h2>
    {#if description}
      <p class="description">{description}</p>
    {/if}
  </header>
  <div class="body">
    {#if children}
      {@render children()}
    {/if}
  </div>
</section>

<style>
  .section {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  header {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  h2 {
    font-size: 15px;
    font-weight: 600;
  }

  .description {
    font-size: 13px;
    color: var(--text-muted);
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
</style>
