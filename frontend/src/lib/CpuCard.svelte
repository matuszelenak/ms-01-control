<script lang="ts">
  import type { CpuProfile, CpuState } from './types'

  interface Props {
    cpu: CpuState
    busy: boolean
    warnings: string[]
    onselect: (profile: CpuProfile | null) => void
  }

  let { cpu, busy, warnings, onselect }: Props = $props()

  const profiles: { value: CpuProfile; label: string; hint: string }[] = [
    {
      value: 'performance',
      label: 'Performance',
      hint: 'performance governor · EPP performance · turbo on',
    },
    {
      value: 'balanced',
      label: 'Balanced',
      hint: 'powersave governor · EPP balance_performance · turbo on',
    },
    {
      value: 'efficiency',
      label: 'Efficiency',
      hint: 'powersave governor · EPP power · turbo off',
    },
  ]

  let supported = $derived(new Set(cpu.supported_profiles))
  let active = $derived(cpu.profile ?? cpu.effective)
</script>

<section class="card">
  <header>
    <h2>CPU power profile</h2>
    <span class="driver">
      {cpu.driver ?? 'no cpufreq'}
      {#if cpu.policy_count > 0}· {cpu.policy_count} CPUs{/if}
    </span>
  </header>

  {#if cpu.policy_count === 0}
    <p class="note">This system exposes no cpufreq interface, so the profile cannot be changed.</p>
  {:else}
    <div class="profiles" role="radiogroup" aria-label="CPU power profile">
      {#each profiles as p}
        <button
          class="profile"
          class:active={active === p.value}
          class:managed={cpu.profile === p.value}
          role="radio"
          aria-checked={active === p.value}
          disabled={busy || !supported.has(p.value)}
          title={supported.has(p.value) ? p.hint : `${p.label} is not supported by this CPU`}
          onclick={() => onselect(p.value)}
        >
          <span class="name">{p.label}</span>
          <span class="hint">{p.hint}</span>
        </button>
      {/each}
    </div>

    <div class="state">
      <span class="item">
        governor <b>{cpu.governor ?? '—'}</b>
      </span>
      <span class="item">
        EPP <b>{cpu.epp ?? 'n/a'}</b>
      </span>
      <span class="item">
        turbo
        <b class:on={cpu.turbo === true} class:off={cpu.turbo === false}>
          {cpu.turbo === null ? 'n/a' : cpu.turbo ? 'on' : 'off'}
        </b>
      </span>
      {#if cpu.profile}
        <span class="badge managed-badge">managed by fancurve</span>
        <button class="unmanage" disabled={busy} onclick={() => onselect(null)}>
          stop managing
        </button>
      {:else}
        <span class="badge">not managed</span>
      {/if}
    </div>

    {#if cpu.profile === null}
      <p class="note">
        No profile is enforced: the settings above are the system's own. Pick one to have
        them applied at every start-up.
      </p>
    {:else if cpu.effective === null}
      <p class="note">
        The live settings do not match the selected profile — the firmware or another tool may
        have changed them. Select the profile again to re-apply it.
      </p>
    {/if}
  {/if}

  {#each warnings as warning}
    <p class="warning">{warning}</p>
  {/each}
</section>

<style>
  .card {
    background: #1a2029;
    border: 1px solid #2a3140;
    border-radius: 12px;
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 10px;
  }
  h2 {
    margin: 0;
    font-size: 15px;
  }
  .driver {
    font-size: 11px;
    color: #7d8799;
  }
  .profiles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
    gap: 8px;
  }
  .profile {
    font: inherit;
    text-align: left;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px 10px;
    border-radius: 9px;
    border: 1px solid #2a3140;
    background: #121821;
    color: #9aa7ba;
    cursor: pointer;
  }
  .profile:hover:not(:disabled) {
    border-color: #3a4557;
    color: #dbe6f5;
  }
  .profile.active {
    border-color: #4da3ff;
    color: #dbe6f5;
    background: #16222f;
  }
  .profile.managed .name::after {
    content: ' ●';
    color: #4da3ff;
    font-size: 9px;
    vertical-align: middle;
  }
  .profile:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .name {
    font-size: 13px;
    font-weight: 700;
  }
  .hint {
    font-size: 10px;
    color: #7d8799;
  }
  .state {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
    align-items: center;
    font-size: 11.5px;
    color: #7d8799;
  }
  .state b {
    color: #c7d1e0;
    font-weight: 600;
  }
  .state b.on {
    color: #6fd388;
  }
  .state b.off {
    color: #9aa7ba;
  }
  .badge {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    border: 1px solid #2a3140;
    border-radius: 999px;
    padding: 1px 8px;
    color: #7d8799;
  }
  .unmanage {
    font: inherit;
    font-size: 11px;
    color: #7d8799;
    background: none;
    border: none;
    padding: 0;
    text-decoration: underline dotted;
    cursor: pointer;
  }
  .unmanage:hover:not(:disabled) {
    color: #dbe6f5;
  }
  .badge.managed-badge {
    border-color: #2f4a63;
    color: #4da3ff;
  }
  .note {
    margin: 0;
    font-size: 11.5px;
    color: #7d8799;
  }
  .warning {
    margin: 0;
    font-size: 11.5px;
    color: #ffb454;
  }
</style>
