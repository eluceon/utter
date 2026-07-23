<script lang="ts">
  import Section from '../lib/components/Section.svelte'
  import TextInput from '../lib/components/TextInput.svelte'
  import { settingsStore } from '../lib/stores'

  let settings = $derived($settingsStore!)

  let newTrigger = $state('')
  let newBody = $state('')

  function addSnippet() {
    const trigger = newTrigger.trim()
    const body = newBody.trim()
    if (!trigger || !body) return
    settingsStore.patch({ snippets: [...settings.snippets, { trigger, body }] })
    newTrigger = ''
    newBody = ''
  }

  function removeSnippet(index: number) {
    const snippets = settings.snippets.filter((_, i) => i !== index)
    settingsStore.patch({ snippets })
  }

  function onSubmit(e: SubmitEvent) {
    e.preventDefault()
    addSnippet()
  }
</script>

<Section
  title="Voice snippets"
  description={'Say a trigger phrase verbatim and it is replaced by its stored body instead of being transcribed.'}
>
  {#if settings.snippets.length > 0}
    <table>
      <thead>
        <tr>
          <th>Trigger</th>
          <th>Body</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each settings.snippets as snippet, i (snippet.trigger + '|' + i)}
          <tr>
            <td>{snippet.trigger}</td>
            <td class="body-cell">{snippet.body}</td>
            <td>
              <button type="button" aria-label="Remove snippet" onclick={() => removeSnippet(i)}>
                Remove
              </button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
  <form class="add-row" onsubmit={onSubmit}>
    <TextInput placeholder="Trigger phrase…" bind:value={() => newTrigger, (v) => (newTrigger = v)} />
    <span class="arrow">→</span>
    <TextInput placeholder="Body text…" bind:value={() => newBody, (v) => (newBody = v)} />
    <button type="submit" disabled={!newTrigger.trim() || !newBody.trim()}>Add</button>
  </form>
</Section>

<style>
  .add-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .arrow {
    color: var(--text-muted);
  }

  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }

  th {
    text-align: left;
    font-weight: 600;
    padding: var(--space-1) var(--space-2);
    color: var(--text-muted);
    border-bottom: 1px solid var(--border);
  }

  td {
    padding: var(--space-1) var(--space-2);
    border-bottom: 1px solid var(--border);
  }

  .body-cell {
    max-width: 320px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
</style>
