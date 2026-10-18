import type { Config, CpuProfile, CpuState, DriveStatus, HistoryResponse, Status } from './types'

async function check(res: Response): Promise<Response> {
  if (!res.ok) {
    const text = await res.text().catch(() => res.statusText)
    throw new Error(`${res.status}: ${text}`)
  }
  return res
}

export async function fetchStatus(): Promise<Status> {
  return (await check(await fetch('/api/status'))).json()
}

export async function fetchConfig(): Promise<Config> {
  return (await check(await fetch('/api/config'))).json()
}

export async function saveConfig(config: Config): Promise<Config> {
  return (
    await check(
      await fetch('/api/config', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(config),
      }),
    )
  ).json()
}

/** Apply a CPU power profile immediately (it is persisted server-side). */
export async function setCpuProfile(
  profile: CpuProfile | null,
): Promise<{ cpu: CpuState; warnings: string[] }> {
  return (
    await check(
      await fetch('/api/cpu', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ profile }),
      }),
    )
  ).json()
}

/** Read SMART data now (also stores a history sample). */
export async function refreshDrives(): Promise<DriveStatus[]> {
  return (await check(await fetch('/api/drives/refresh', { method: 'POST' }))).json()
}

export async function fetchHistory(days: number): Promise<HistoryResponse> {
  return (await check(await fetch(`/api/history?days=${days}`))).json()
}
