<script lang="ts">
  import { onDestroy, onMount } from 'svelte'
  import { listen, type UnlistenFn } from '@tauri-apps/api/event'
  import { invoke } from '@tauri-apps/api/core'

  import { HUD_STYLE } from './layout'

  type Phase = 'idle' | 'recording' | 'transcribing' | 'refining' | 'injecting'

  interface DictationStatePayload {
    state: Phase
    level: number
    partial: string | null
  }

  const BAR_COUNT = 12
  const barIndices = Array.from({ length: BAR_COUNT }, (_, i) => i)

  const STATE_LABEL: Record<Phase, string> = {
    idle: 'Idle',
    recording: 'Listening',
    transcribing: 'Transcribing',
    refining: 'Refining',
    injecting: 'Injecting',
  }

  let phase = $state<Phase>('idle')
  let level = $state(0)
  let partial = $state<string | null>(null)

  // `level` is an RMS amplitude in 0..1 (see `utter_audio::rms_level`); the
  // count of "lit" bars is how it's surfaced here, rather than driving each
  // bar's height individually — a steadier, less jittery read at a glance.
  let activeBars = $derived(Math.round(Math.min(1, Math.max(0, level)) * BAR_COUNT))

  let unlisten: UnlistenFn | undefined

  onMount(() => {
    listen<DictationStatePayload>('dictation-state', (event) => {
      phase = event.payload.state
      level = event.payload.level
      partial = event.payload.partial
    }).then((fn) => {
      unlisten = fn
    })
  })

  onDestroy(() => {
    unlisten?.()
  })

  function cancel() {
    // Best-effort: a HUD click that can't reach the runtime (e.g. it never
    // booted) shouldn't throw in the UI.
    invoke('cancel_dictation').catch(() => {})
  }

  function cancelOnKey(event: KeyboardEvent) {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault()
      cancel()
    }
  }
</script>

<div
  class="hud"
  style={HUD_STYLE}
  data-state={phase}
  role="button"
  tabindex="0"
  onclick={cancel}
  onkeydown={cancelOnKey}
>
  <div class="row">
    <span class="dot"></span>
    <span class="label">{STATE_LABEL[phase]}</span>
  </div>
  <div class="bars">
    {#each barIndices as i (i)}
      <span class="bar" class:active={i < activeBars}></span>
    {/each}
  </div>
  {#if partial}
    <div class="partial"><span>{partial}</span></div>
  {/if}
</div>

<style>
  :global(html),
  :global(body) {
    background: transparent;
  }

  /* The pill sizes itself from its rows (see `./layout.ts`) rather than to a
     pinned height: the window is sized for the pill's tallest state, the
     live preview, and is transparent everywhere the pill isn't.

     Pinning the height here is what hid the preview. A fixed-height flex
     column does not overflow when its rows don't fit — it *shrinks* them,
     and this column's rows fitted only until a third one existed. The
     preview row was squeezed to a few pixels and then clipped away by its
     own `overflow: hidden`: present in the DOM, correct in every event it
     received, and never once drawn. */
  .hud {
    box-sizing: border-box;
    width: 280px;
    padding: var(--hud-pad-y) 14px;
    display: flex;
    flex-direction: column;
    gap: var(--hud-row-gap);
    border-radius: 16px;
    background: rgba(18, 18, 22, 0.86);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.35);
    color: rgba(255, 255, 255, 0.92);
    font-family:
      -apple-system,
      BlinkMacSystemFont,
      'Segoe UI',
      system-ui,
      sans-serif;
    cursor: pointer;
    user-select: none;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--hud-status-row);
  }

  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #888;
    flex-shrink: 0;
    transition: background-color 150ms ease;
  }

  .hud[data-state='recording'] .dot {
    background: #ff5c5c;
    box-shadow: 0 0 6px rgba(255, 92, 92, 0.7);
  }

  .hud[data-state='transcribing'] .dot {
    background: #f5a623;
    box-shadow: 0 0 6px rgba(245, 166, 35, 0.7);
  }

  .hud[data-state='refining'] .dot {
    background: #9b59f6;
    box-shadow: 0 0 6px rgba(155, 89, 246, 0.7);
  }

  .hud[data-state='injecting'] .dot {
    background: #2ecc71;
    box-shadow: 0 0 6px rgba(46, 204, 113, 0.7);
  }

  .label {
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    opacity: 0.85;
  }

  .bars {
    display: flex;
    align-items: flex-end;
    gap: 3px;
    height: var(--hud-meter);
    flex-shrink: 0;
  }

  .bar {
    flex: 1;
    height: 20%;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.18);
    transition:
      height 80ms ease,
      background-color 80ms ease;
  }

  .bar:nth-child(3n + 1) {
    height: 35%;
  }
  .bar:nth-child(3n + 2) {
    height: 65%;
  }
  .bar:nth-child(4n) {
    height: 90%;
  }

  .bar.active {
    background: rgba(255, 255, 255, 0.55);
  }

  .hud[data-state='recording'] .bar.active {
    background: #ff5c5c;
  }

  .hud[data-state='transcribing'] .bar.active {
    background: #f5a623;
  }

  .hud[data-state='refining'] .bar.active {
    background: #9b59f6;
  }

  .hud[data-state='injecting'] .bar.active {
    background: #2ecc71;
  }

  /* A live preview only ever grows, so the interesting end is the newest
     one. The row is a fixed two-line box (never taller, never shorter, so a
     window sitting over the user's work never resizes as they speak) with
     its text pinned to the bottom: as the sentence outgrows two lines the
     older lines scroll off the top, whole lines at a time, because the box
     is an exact multiple of the line box. Ellipsising the *end* instead —
     what `text-overflow` does — would have frozen the preview on the first
     few words it ever showed. */
  .partial {
    height: var(--hud-partial);
    display: flex;
    align-items: flex-end;
    overflow: hidden;
    font-size: 12px;
    line-height: var(--hud-partial-line);
    opacity: 0.9;
  }

  .partial span {
    flex: 1;
    overflow-wrap: anywhere;
  }
</style>
