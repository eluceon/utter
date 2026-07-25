<script lang="ts">
  import Section from '../lib/components/Section.svelte'
  import Field from '../lib/components/Field.svelte'
  import TextInput from '../lib/components/TextInput.svelte'
  import { settingsStore } from '../lib/stores'

  let settings = $derived($settingsStore!)

  let newTerm = $state('')
  let newRuleHeard = $state('')
  let newRuleWrite = $state('')

  function addTerm() {
    const term = newTerm.trim()
    if (!term) return
    settingsStore.patch({ dictionary: { terms: [...settings.dictionary.terms, term] } })
    newTerm = ''
  }

  function removeTerm(index: number) {
    const terms = settings.dictionary.terms.filter((_, i) => i !== index)
    settingsStore.patch({ dictionary: { terms } })
  }

  function addRule() {
    const heard = newRuleHeard.trim()
    const write = newRuleWrite.trim()
    if (!heard || !write) return
    settingsStore.patch({ dictionary: { rules: [...settings.dictionary.rules, { heard, write }] } })
    newRuleHeard = ''
    newRuleWrite = ''
  }

  function removeRule(index: number) {
    const rules = settings.dictionary.rules.filter((_, i) => i !== index)
    settingsStore.patch({ dictionary: { rules } })
  }

  function onAddTermSubmit(e: SubmitEvent) {
    e.preventDefault()
    addTerm()
  }

  function onAddRuleSubmit(e: SubmitEvent) {
    e.preventDefault()
    addRule()
  }
</script>

<Section
  title="Custom terms"
  description="Words and names hinted to the speech-to-text engine to improve recognition."
>
  <ul class="chip-list">
    {#each settings.dictionary.terms as term, i (term + i)}
      <li class="chip">
        <span>{term}</span>
        <button type="button" aria-label="Remove {term}" onclick={() => removeTerm(i)}>×</button>
      </li>
    {/each}
  </ul>
  <form class="add-row" onsubmit={onAddTermSubmit}>
    <TextInput placeholder="Add a term…" bind:value={() => newTerm, (v) => (newTerm = v)} />
    <button type="submit" disabled={!newTerm.trim()}>Add</button>
  </form>
</Section>

<Section title="Replacement rules" description={'"heard X" is replaced with "write Y" before refinement.'}>
  {#if settings.dictionary.rules.length > 0}
    <table>
      <thead>
        <tr>
          <th>Heard</th>
          <th>Write</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each settings.dictionary.rules as rule, i (rule.heard + '|' + rule.write + i)}
          <tr>
            <td>{rule.heard}</td>
            <td>{rule.write}</td>
            <td>
              <button type="button" aria-label="Remove rule" onclick={() => removeRule(i)}>Remove</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
  <form class="add-row" onsubmit={onAddRuleSubmit}>
    <TextInput placeholder="Heard…" bind:value={() => newRuleHeard, (v) => (newRuleHeard = v)} />
    <span class="arrow">→</span>
    <TextInput placeholder="Write…" bind:value={() => newRuleWrite, (v) => (newRuleWrite = v)} />
    <button type="submit" disabled={!newRuleHeard.trim() || !newRuleWrite.trim()}>Add</button>
  </form>
</Section>

<style>
  .chip-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .chip {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    padding: 4px var(--space-2);
    border-radius: 999px;
    background: var(--bg-sunken);
    font-size: 13px;
  }

  .chip button {
    border: none;
    background: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
    padding: 0 2px;
  }

  .chip button:hover {
    color: var(--danger);
  }

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
