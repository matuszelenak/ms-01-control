<script lang="ts">
  import { onMount } from 'svelte'
  import CpuCard from './lib/CpuCard.svelte'
  import DriveCard from './lib/DriveCard.svelte'
  import FanCard from './lib/FanCard.svelte'
  import { fetchConfig, fetchHistory, fetchStatus, refreshDrives, saveConfig, setCpuProfile } from './lib/api'
  import type { Config, CpuProfile, FanConfig, HistoryPoint, Status } from './lib/types'

  let config = $state<Config | null>(null)
  let savedConfig = $state('') // JSON snapshot of the last saved config
  let status = $state<Status | null>(null)
  let error: string | null = $state(null)
  let saving = $state(false)

  // Drive wear history, keyed by device name.
  let history: Record<string, HistoryPoint[]> = $state({})
  let historyDays = $state(90)
  let historyError: string | null = $state(null)
  let refreshing = $state(false)

  // CPU profile switching.
  let cpuBusy = $state(false)
  let cpuWarnings: string[] = $state([])

  let dirty = $derived(config !== null && JSON.stringify(config) !== savedConfig)
  let cpuPackage = $derived(status?.sensors.find((s) => s.kind === 'cpu') ?? null)
  let cores = $derived(status?.sensors.filter((s) => s.kind === 'core') ?? [])
  let nvmeSensors = $derived(status?.sensors.filter((s) => s.kind === 'nvme') ?? [])
  let drives = $derived(status?.drives ?? [])

  async function pollStatus() {
    try {
      status = await fetchStatus()
      error = null
    } catch (e) {
      error = String(e)
    }
  }

  async function loadHistory() {
    try {
      const response = await fetchHistory(historyDays)
      const map: Record<string, HistoryPoint[]> = {}
      for (const series of response.series) map[series.device] = series.points
      history = map
      historyError = null
    } catch (e) {
      historyError = String(e)
    }
  }

  onMount(() => {
    fetchConfig()
      .then((c) => {
        config = c
        savedConfig = JSON.stringify(c)
      })
      .catch((e) => (error = String(e)))

    pollStatus()
    loadHistory()

    const statusTimer = setInterval(pollStatus, 2000)
    const historyTimer = setInterval(loadHistory, 5 * 60 * 1000)
    return () => {
      clearInterval(statusTimer)
      clearInterval(historyTimer)
    }
  })

  function updateFan(i: number, fan: FanConfig) {
    if (!config) return
    const fans = config.fans.map((f, j) => (j === i ? fan : f))
    config = { ...config, fans }
  }

  async function save() {
    if (!config) return
    saving = true
    try {
      const applied = await saveConfig(config)
      config = applied
      savedConfig = JSON.stringify(applied)
      error = null
    } catch (e) {
      error = String(e)
    } finally {
      saving = false
    }
  }

  function revert() {
    if (savedConfig) config = JSON.parse(savedConfig)
  }

  async function selectCpu(profile: CpuProfile | null) {
    cpuBusy = true
    try {
      const result = await setCpuProfile(profile)
      cpuWarnings = result.warnings
      if (status) status = { ...status, cpu: result.cpu }
      // The server persists the profile right away; keep the config snapshot
      // (and hence the "unsaved changes" indicator) in sync without touching
      // pending fan edits.
      if (config) config = { ...config, cpu: { profile: result.cpu.profile } }
      if (savedConfig) {
        const saved = JSON.parse(savedConfig) as Config
        savedConfig = JSON.stringify({ ...saved, cpu: { profile: result.cpu.profile } })
      }
      error = null
    } catch (e) {
      error = String(e)
    } finally {
      cpuBusy = false
    }
  }

  function setHistoryDays(event: Event) {
    historyDays = Number((event.currentTarget as HTMLSelectElement).value)
    loadHistory()
  }

  async function refreshWear() {
    refreshing = true
    try {
      const updated = await refreshDrives()
      if (status) status = { ...status, drives: updated }
      await loadHistory()
      error = null
    } catch (e) {
      error = String(e)
    } finally {
      refreshing = false
    }
  }

  function driveTemp(device: string): number | null {
    return nvmeSensors.find((s) => s.label === device)?.temp ?? null
  }

  function tempClass(t: number | null): string {
    if (t == null) return ''
    if (t >= 80) return 'hot'
    if (t >= 65) return 'warm'
    return 'cool'
  }
</script>

<main>
  <header class="top">
    <div>
      <h1>MS-01 Control</h1>
      <p class="subtitle">fans · drive wear · CPU power profile</p>
    </div>
    <div class="actions">
      {#if dirty}
        <span class="dirty-badge">unsaved changes</span>
        <button class="secondary" onclick={revert}>Revert</button>
      {/if}
      <button class="primary" disabled={!dirty || saving} onclick={save}>
        {saving ? 'Saving…' : 'Save & Apply'}
      </button>
    </div>
  </header>

  {#if error}
    <div class="error">{error}</div>
  {/if}

  <section class="sensor-group">
    <h3>CPU <span class="group-note">controls the fans</span></h3>
    <div class="temps">
      {#if cpuPackage}
        <div class="temp control {tempClass(cpuPackage.temp)}">
          <span class="label">{cpuPackage.label}</span>
          <span class="value">{cpuPackage.temp != null ? cpuPackage.temp.toFixed(1) + '°C' : '—'}</span>
        </div>
      {/if}
      {#each cores as s}
        <div class="temp small {tempClass(s.temp)}">
          <span class="label">{s.label}</span>
          <span class="value">{s.temp != null ? s.temp.toFixed(0) + '°' : '—'}</span>
        </div>
      {/each}
    </div>
  </section>

  {#if status}
    <CpuCard cpu={status.cpu} busy={cpuBusy} warnings={cpuWarnings} onselect={selectCpu} />
  {/if}

  <section class="drives">
    <header class="section-head">
      <h3>NVMe drives <span class="group-note">SMART endurance</span></h3>
      <div class="section-actions">
        <select value={historyDays} onchange={setHistoryDays} title="History window">
          <option value={7}>7 days</option>
          <option value={30}>30 days</option>
          <option value={90}>90 days</option>
          <option value={365}>1 year</option>
          <option value={1095}>3 years</option>
        </select>
        <button class="secondary small" disabled={refreshing} onclick={refreshWear}>
          {refreshing ? 'Reading…' : 'Refresh'}
        </button>
      </div>
    </header>

    {#if historyError}
      <p class="hint">History unavailable: {historyError}</p>
    {:else if status && !status.history_db}
      <p class="hint">History is disabled (no database configured) — only live readings are shown.</p>
    {/if}

    {#if drives.length > 0}
      <div class="drive-grid">
        {#each drives as drive (drive.device)}
          <DriveCard
            {drive}
            temp={driveTemp(drive.device)}
            history={history[drive.device] ?? []}
            rangeDays={historyDays}
          />
        {/each}
      </div>
    {:else if nvmeSensors.length > 0}
      <!-- No /sys/class/nvme (e.g. container): fall back to plain temperatures. -->
      <div class="temps">
        {#each nvmeSensors as s}
          <div class="temp {tempClass(s.temp)}">
            <span class="label" title={s.model ?? s.label}>{s.model ?? s.label}</span>
            <span class="sublabel">{s.label}</span>
            <span class="value">{s.temp != null ? s.temp.toFixed(1) + '°C' : '—'}</span>
          </div>
        {/each}
      </div>
    {:else}
      <p class="hint">No NVMe drives detected.</p>
    {/if}
  </section>

  {#if config}
    {#if status && status.fans.length === 0}
      <div class="notice">
        No fan controller detected (nct67xx hwmon chip missing) — fan control is disabled,
        the monitoring sections above still work.
      </div>
    {/if}
    <div class="fans">
      {#each config.fans as fan, i}
        <FanCard
          index={i + 1}
          config={fan}
          status={status?.fans[i] ?? null}
          liveTemp={status?.control_temp ?? null}
          onchange={(f) => updateFan(i, f)}
        />
      {/each}
    </div>
  {:else if !error}
    <p class="loading">Loading configuration…</p>
  {/if}
</main>

<style>
  main {
    max-width: 980px;
    margin: 0 auto;
    padding: 20px 16px 48px;
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .top {
    display: flex;
    justify-content: space-between;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px;
  }
  h1 {
    margin: 0;
    font-size: 22px;
  }
  .subtitle {
    margin: 2px 0 0;
    font-size: 12px;
    color: #7d8799;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .dirty-badge {
    font-size: 12px;
    color: #ffb454;
  }
  button {
    font: inherit;
    border-radius: 8px;
    padding: 8px 18px;
    cursor: pointer;
    border: 1px solid #2a3140;
  }
  button.primary {
    background: #4da3ff;
    border-color: #4da3ff;
    color: #0c1117;
    font-weight: 600;
  }
  button.primary:disabled {
    background: #2a3140;
    border-color: #2a3140;
    color: #7d8799;
    cursor: default;
  }
  button.secondary {
    background: transparent;
    color: #9aa7ba;
  }
  button.secondary:hover:not(:disabled) {
    color: #dbe6f5;
  }
  button.secondary:disabled {
    color: #5b6577;
    cursor: default;
  }
  button.small {
    padding: 4px 12px;
    font-size: 12px;
  }
  .error {
    background: #3a1d1d;
    border: 1px solid #7c3030;
    color: #ff9c9c;
    border-radius: 8px;
    padding: 10px 14px;
    font-size: 14px;
  }
  .notice {
    background: #2a2418;
    border: 1px solid #5c4a22;
    color: #ffb454;
    border-radius: 8px;
    padding: 10px 14px;
    font-size: 13px;
  }
  .sensor-group h3,
  .section-head h3 {
    margin: 0 0 8px;
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: #9aa7ba;
  }
  .group-note {
    text-transform: none;
    letter-spacing: 0;
    font-weight: 400;
    font-size: 12px;
    color: #7d8799;
    margin-left: 6px;
  }
  .temps {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    align-items: stretch;
  }
  .temp {
    background: #1a2029;
    border: 1px solid #2a3140;
    border-radius: 10px;
    padding: 8px 14px;
    display: flex;
    flex-direction: column;
    min-width: 110px;
  }
  .temp.small {
    min-width: 56px;
    padding: 5px 10px;
  }
  .temp.small .value {
    font-size: 14px;
  }
  .temp.control {
    border-color: #ff9c41;
  }
  .temp .label {
    font-size: 11px;
    color: #7d8799;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 200px;
  }
  .temp .sublabel {
    font-size: 10px;
    color: #5b6577;
    white-space: nowrap;
  }
  .temp .value {
    font-size: 18px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
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
  .section-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .section-head h3 {
    margin-bottom: 0;
  }
  .section-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .section-actions select {
    font: inherit;
    font-size: 12px;
    color: #c7d1e0;
    background: #121821;
    border: 1px solid #2a3140;
    border-radius: 8px;
    padding: 4px 8px;
  }
  .drive-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
    gap: 12px;
    margin-top: 10px;
  }
  .hint {
    margin: 6px 0 0;
    font-size: 12px;
    color: #7d8799;
  }
  .fans {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .loading {
    color: #7d8799;
  }
</style>
