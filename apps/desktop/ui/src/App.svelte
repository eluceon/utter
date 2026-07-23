<script lang="ts">
  import { onMount } from 'svelte'
  import { invoke } from '@tauri-apps/api/core'

  // Minimal placeholder UI for Task 16. The full settings UI lands in
  // Task 19; this just proves the `get_settings` command round-trips.
  let status = $state('loading settings...')

  onMount(async () => {
    try {
      const settings = await invoke('get_settings')
      status = `settings loaded (hotkey: ${(settings as any)?.dictation?.hotkey ?? 'unknown'})`
    } catch (err) {
      status = `failed to load settings: ${err}`
    }
  })
</script>

<main>
  <h1>Utter</h1>
  <p>{status}</p>
</main>

<style>
  main {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100vh;
    font-family: system-ui, sans-serif;
    color: #333;
  }

  h1 {
    font-weight: 600;
  }
</style>
