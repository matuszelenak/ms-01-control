<script lang="ts">
  interface Props {
    /** Values ordered oldest → newest. Fewer than two values render nothing. */
    values: number[]
    height?: number
    color?: string
    /** Accessible description, e.g. "wear percentage history". */
    label?: string
  }

  let { values, height = 46, color = '#4da3ff', label = 'history' }: Props = $props()

  let geometry = $derived.by(() => {
    if (values.length < 2) return null
    const min = Math.min(...values)
    const max = Math.max(...values)
    const span = max - min || 1
    const flat = max === min
    const points = values.map((v, i) => {
      const x = (i / (values.length - 1)) * 100
      // A flat series has no span; draw it through the middle.
      const y = flat ? height / 2 : height - 3 - ((v - min) / span) * (height - 6)
      return [x, y] as const
    })
    const line = points
      .map(([x, y], i) => `${i === 0 ? 'M' : 'L'}${x.toFixed(2)} ${y.toFixed(2)}`)
      .join(' ')
    const area = `${line} L100 ${height} L0 ${height} Z`
    const last = points[points.length - 1]
    return { line, area, last, min, max }
  })
</script>

{#if geometry}
  <svg
    class="sparkline"
    viewBox="0 0 100 {height}"
    preserveAspectRatio="none"
    role="img"
    aria-label="{label}: {geometry.min} to {geometry.max}"
  >
    <path d={geometry.area} fill={color} opacity="0.13" />
    <path
      d={geometry.line}
      fill="none"
      stroke={color}
      stroke-width="1.5"
      stroke-linejoin="round"
      vector-effect="non-scaling-stroke"
    />
    <circle cx={geometry.last[0]} cy={geometry.last[1]} r="1.6" fill={color} />
  </svg>
{:else}
  <div class="sparkline empty">not enough history yet</div>
{/if}

<style>
  .sparkline {
    display: block;
    width: 100%;
    height: 46px;
  }
  .sparkline.empty {
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 11px;
    color: #5b6577;
    border: 1px dashed #232a36;
    border-radius: 8px;
  }
</style>
