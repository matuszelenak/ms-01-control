<script lang="ts">
  import Sparkline from './Sparkline.svelte'
  import type { DriveStatus, HistoryPoint } from './types'

  interface Props {
    drive: DriveStatus
    /** Composite temperature from the hwmon sensor with the same label. */
    temp: number | null
    history: HistoryPoint[]
    /** Length of the history window in days. */
    rangeDays: number
  }

  let { drive, temp, history, rangeDays }: Props = $props()

  let wear = $derived(drive.wear)
  let wearPct = $derived(wear?.percentage_used ?? null)
  let barWidth = $derived(wearPct == null ? 0 : Math.min(100, wearPct))
  let wearClass = $derived(
    wearPct == null ? '' : wearPct >= 80 ? 'hot' : wearPct >= 50 ? 'warm' : 'cool',
  )
  let tempClass = $derived(temp == null ? '' : temp >= 70 ? 'hot' : temp >= 55 ? 'warm' : 'cool')

  let points = $derived(history.filter((p) => p.percentage_used != null))
  let values = $derived(points.map((p) => p.percentage_used as number))

  /** Wear rate derived from the stored history, and the projected lifetime. */
  let projection = $derived.by(() => {
    if (points.length < 2 || wearPct == null) return null
    const first = points[0]
    const last = points[points.length - 1]
    const days = (last.ts - first.ts) / 86_400
    const delta = (last.percentage_used ?? 0) - (first.percentage_used ?? 0)
    if (days < 2 || delta <= 0) return null
    const perMonth = (delta / days) * 30.44
    const remainingMonths = perMonth > 0 ? (100 - wearPct) / perMonth : null
    return { perMonth, remainingMonths }
  })

  function fmtBytes(bytes: number | null | undefined): string {
    if (bytes == null) return '—'
    const tb = bytes / 1e12
    if (tb >= 1) return `${tb.toFixed(2)} TB`
    return `${(bytes / 1e9).toFixed(0)} GB`
  }

  function fmtHours(hours: number | null | undefined): string {
    if (hours == null) return '—'
    if (hours < 48) return `${hours} h`
    const days = hours / 24
    if (days < 730) return `${days.toFixed(0)} d`
    return `${(days / 365).toFixed(1)} y`
  }

  function fmtDate(ts: number | null): string {
    if (ts == null) return 'never'
    return new Date(ts * 1000).toLocaleString()
  }

  function fmtMonths(months: number | null): string {
    if (months == null || !Number.isFinite(months)) return '—'
    if (months > 120) return '100+ years'
    return `${months.toFixed(0)} months`
  }
</script>

<section class="drive">
  <header>
    <div class="title">
      <h3>{drive.model ?? drive.device}</h3>
      <span class="sub">
        {drive.device}{drive.serial ? ` · ${drive.serial}` : ''}{drive.firmware ? ` · fw ${drive.firmware}` : ''}
      </span>
    </div>
    <div class="temp {tempClass}">
      <span class="value">{temp != null ? temp.toFixed(0) : '—'}</span>
      <span class="unit">°C</span>
    </div>
  </header>

  {#if wear}
    <div class="wear">
      <div class="wear-head">
        <span class="label">Wear</span>
        <span class="pct {wearClass}">{wearPct != null ? `${wearPct}%` : '—'}</span>
      </div>
      <div class="bar"><div class="fill {wearClass}" style="width: {barWidth}%"></div></div>
      <div class="wear-foot">
        <span>{100 - barWidth}% endurance left</span>
        <span title="Source and time of the reading">
          {wear.source} · {fmtDate(drive.sampled_at)}
        </span>
      </div>
    </div>

    <dl class="stats">
      <div>
        <dt>Written</dt>
        <dd>{fmtBytes(wear.bytes_written)}</dd>
      </div>
      <div>
        <dt>Power-on</dt>
        <dd>{fmtHours(wear.power_on_hours)}</dd>
      </div>
      <div>
        <dt>Spare</dt>
        <dd>{wear.available_spare != null ? `${wear.available_spare}%` : '—'}</dd>
      </div>
      <div>
        <dt>Media errors</dt>
        <dd class:warn={wear.media_errors != null && wear.media_errors > 0}>
          {wear.media_errors ?? '—'}
        </dd>
      </div>
      <div>
        <dt>Unsafe shutdowns</dt>
        <dd>{wear.unsafe_shutdowns ?? '—'}</dd>
      </div>
      <div>
        <dt>Power cycles</dt>
        <dd>{wear.power_cycles ?? '—'}</dd>
      </div>
    </dl>

    {#if wear.critical_warning != null && wear.critical_warning > 0}
      <p class="alert">SMART critical warning: {wear.critical_warning}</p>
    {/if}

    <div class="history">
      <div class="history-head">
        <span>Wear over the last {rangeDays} days</span>
        {#if projection}
          <span class="rate">
            {projection.perMonth.toFixed(2)} %/month · ≈{fmtMonths(projection.remainingMonths)} to 100%
          </span>
        {:else}
          <span class="rate muted">no trend yet</span>
        {/if}
      </div>
      <Sparkline
        {values}
        color={wearClass === 'hot' ? '#ff6b6b' : wearClass === 'warm' ? '#ffb454' : '#4da3ff'}
        label="wear percentage of {drive.device}"
      />
    </div>
  {:else}
    <p class="alert">
      Wear data unavailable{drive.wear_error ? `: ${drive.wear_error}` : ''}
    </p>
  {/if}
</section>

<style>
  .drive {
    background: #1a2029;
    border: 1px solid #2a3140;
    border-radius: 12px;
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
  }
  .title {
    min-width: 0;
  }
  h3 {
    margin: 0;
    font-size: 15px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sub {
    font-size: 11px;
    color: #7d8799;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    display: block;
  }
  .temp {
    display: flex;
    align-items: baseline;
    gap: 2px;
    font-variant-numeric: tabular-nums;
  }
  .temp .value {
    font-size: 20px;
    font-weight: 700;
  }
  .temp .unit {
    font-size: 11px;
    color: #7d8799;
  }
  .temp.cool .value {
    color: #6fd388;
  }
  .temp.warm .value {
    color: #ffb454;
  }
  .temp.hot .value {
    color: #ff6b6b;
  }
  .wear-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    margin-bottom: 5px;
  }
  .label {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: #9aa7ba;
  }
  .pct {
    font-size: 18px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }
  .pct.cool {
    color: #6fd388;
  }
  .pct.warm {
    color: #ffb454;
  }
  .pct.hot {
    color: #ff6b6b;
  }
  .bar {
    height: 6px;
    border-radius: 999px;
    background: #121821;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    border-radius: 999px;
    background: #4da3ff;
  }
  .fill.cool {
    background: #4da3ff;
  }
  .fill.warm {
    background: #ffb454;
  }
  .fill.hot {
    background: #ff6b6b;
  }
  .wear-foot {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    font-size: 10.5px;
    color: #5b6577;
    margin-top: 5px;
  }
  .stats {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(80px, 1fr));
    gap: 8px 12px;
    margin: 0;
  }
  .stats dt {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: #7d8799;
  }
  .stats dd {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .stats dd.warn {
    color: #ffb454;
  }
  .history-head {
    display: flex;
    justify-content: space-between;
    gap: 10px;
    font-size: 11px;
    color: #7d8799;
    margin-bottom: 4px;
  }
  .rate {
    color: #9aa7ba;
    white-space: nowrap;
  }
  .rate.muted {
    color: #5b6577;
  }
  .alert {
    margin: 0;
    font-size: 12px;
    color: #ffb454;
    word-break: break-word;
  }
</style>
