<script lang="ts">
  import { onDestroy, onMount } from 'svelte'

  import { settingsStore } from './lib/stores'
  import { applyTheme } from './lib/theme'

  import General from './pages/General.svelte'
  import Dictation from './pages/Dictation.svelte'
  import Engines from './pages/Engines.svelte'
  import Refinement from './pages/Refinement.svelte'
  import DictionaryPage from './pages/Dictionary.svelte'
  import Snippets from './pages/Snippets.svelte'
  import History from './pages/History.svelte'
  import Advanced from './pages/Advanced.svelte'

  const NAV: { hash: string; label: string }[] = [
    { hash: 'general', label: 'General' },
    { hash: 'dictation', label: 'Dictation' },
    { hash: 'engines', label: 'Engines' },
    { hash: 'refinement', label: 'Refinement' },
    { hash: 'dictionary', label: 'Dictionary' },
    { hash: 'snippets', label: 'Snippets' },
    { hash: 'history', label: 'History' },
    { hash: 'advanced', label: 'Advanced' },
  ]

  function currentHash(): string {
    const raw = window.location.hash.replace(/^#/, '')
    return NAV.some((n) => n.hash === raw) ? raw : 'general'
  }

  let hash = $state(currentHash())
  let loading = $state(true)
  let loadError = $state('')

  function onHashChange() {
    void settingsStore.flush()
    hash = currentHash()
  }

  function onBeforeUnload() {
    void settingsStore.flush()
  }

  onMount(async () => {
    try {
      await settingsStore.load()
    } catch (err) {
      loadError = `Failed to load settings: ${String(err)}`
    } finally {
      loading = false
    }

    window.addEventListener('hashchange', onHashChange)
    window.addEventListener('beforeunload', onBeforeUnload)
  })

  onDestroy(() => {
    window.removeEventListener('hashchange', onHashChange)
    window.removeEventListener('beforeunload', onBeforeUnload)
    // A patch made right before this window closes (e.g. the user tweaked a
    // field and immediately hit the OS close button) must not be dropped
    // just because the 500ms debounce hadn't elapsed yet.
    void settingsStore.flush()
  })

  $effect(() => {
    if ($settingsStore) applyTheme($settingsStore.general.theme)
  })
</script>

{#if loading}
  <div class="status">Loading settings…</div>
{:else if loadError}
  <div class="status error">{loadError}</div>
{:else if $settingsStore}
  <div class="shell">
    <nav aria-label="Settings sections">
      <div class="brand">Utter</div>
      <ul>
        {#each NAV as item (item.hash)}
          <li>
            <a
              href="#{item.hash}"
              aria-current={hash === item.hash ? 'page' : undefined}
              class:active={hash === item.hash}
            >
              {item.label}
            </a>
          </li>
        {/each}
      </ul>
    </nav>
    <main>
      {#if hash === 'general'}
        <General />
      {:else if hash === 'dictation'}
        <Dictation />
      {:else if hash === 'engines'}
        <Engines />
      {:else if hash === 'refinement'}
        <Refinement />
      {:else if hash === 'dictionary'}
        <DictionaryPage />
      {:else if hash === 'snippets'}
        <Snippets />
      {:else if hash === 'history'}
        <History />
      {:else if hash === 'advanced'}
        <Advanced />
      {/if}
    </main>
  </div>
{/if}

<style>
  .status {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100vh;
    color: var(--text-muted);
    font-size: 14px;
  }

  .status.error {
    color: var(--danger);
  }

  .shell {
    display: grid;
    grid-template-columns: 200px 1fr;
    height: 100vh;
  }

  nav {
    background: var(--bg-sunken);
    border-right: 1px solid var(--border);
    padding: var(--space-4) var(--space-2);
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }

  .brand {
    font-weight: 700;
    font-size: 15px;
    padding: 0 var(--space-2);
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  a {
    display: block;
    padding: var(--space-2);
    border-radius: var(--radius-sm);
    color: var(--text);
    text-decoration: none;
    font-size: 13px;
    font-weight: 500;
  }

  a:hover {
    background: var(--bg-elevated);
  }

  a.active {
    background: var(--accent);
    color: var(--accent-contrast);
  }

  main {
    overflow-y: auto;
    padding: var(--space-6);
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    max-width: 760px;
  }
</style>
