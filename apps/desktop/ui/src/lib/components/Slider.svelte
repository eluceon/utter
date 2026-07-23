<script lang="ts">
  interface Props {
    value?: number
    id?: string
    min?: number
    max?: number
    step?: number
    disabled?: boolean
    /** Formats the numeric readout shown next to the slider. */
    format?: (value: number) => string
  }

  let {
    value = $bindable(0),
    id,
    min = 0,
    max = 1,
    step = 0.05,
    disabled = false,
    format = (v) => v.toFixed(2),
  }: Props = $props()
</script>

<div class="slider-row">
  <input
    class="slider"
    type="range"
    {id}
    {min}
    {max}
    {step}
    {disabled}
    bind:value
    aria-valuetext={format(value)}
  />
  <output class="readout" for={id}>{format(value)}</output>
</div>

<style>
  .slider-row {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    max-width: 320px;
  }

  .slider {
    flex: 1;
    accent-color: var(--accent);
    height: 20px;
  }

  .slider:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .readout {
    font-size: 12px;
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
    min-width: 3ch;
    text-align: right;
  }
</style>
