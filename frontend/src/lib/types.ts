export interface CurvePoint {
  temp: number
  pwm: number
}

export type FanMode = 'auto' | 'curve' | 'hardware' | 'manual'

export interface FanConfig {
  mode: FanMode
  manual_pwm: number
  curve: CurvePoint[]
  hw_curve: CurvePoint[]
}

export type CpuProfile = 'performance' | 'balanced' | 'efficiency'

export interface CpuConfig {
  profile: CpuProfile | null
}

export interface Config {
  poll_interval_ms: number
  fans: FanConfig[]
  cpu: CpuConfig
  wear_sample_interval_secs: number
  wear_retention_days: number
}

export interface SensorStatus {
  label: string
  kind: 'cpu' | 'core' | 'nvme' | string
  model?: string
  control: boolean
  temp: number | null
}

export interface FanStatus {
  index: number
  rpm: number | null
  pwm_raw: number | null
  pwm_pct: number | null
  enable: number | null
  mode: FanMode | null
  target_pct: number | null
  hw_points: number
}

/** SMART endurance figures of one NVMe drive. */
export interface WearInfo {
  percentage_used: number | null
  available_spare: number | null
  available_spare_threshold: number | null
  media_errors: number | null
  num_err_log_entries: number | null
  power_on_hours: number | null
  power_cycles: number | null
  unsafe_shutdowns: number | null
  data_units_written: number | null
  bytes_written: number | null
  temperature_c: number | null
  critical_warning: number | null
  /** Tool that produced the reading: `smartctl` or `nvme-cli`. */
  source: string
}

export interface DriveStatus {
  device: string
  model: string | null
  serial: string | null
  firmware: string | null
  wear: WearInfo | null
  wear_error: string | null
  /** Unix seconds of the wear reading. */
  sampled_at: number | null
}

export interface CpuState {
  /** Profile the daemon manages; `null` = not managed. */
  profile: CpuProfile | null
  /** Profile matching the live settings, when they match one exactly. */
  effective: CpuProfile | null
  driver: string | null
  governor: string | null
  epp: string | null
  /** `true` = turbo enabled, `null` = no turbo knob on this platform. */
  turbo: boolean | null
  available_governors: string[]
  available_epp: string[]
  supported_profiles: CpuProfile[]
  policy_count: number
}

export interface Status {
  sensors: SensorStatus[]
  control_temp: number | null
  fans: FanStatus[]
  drives: DriveStatus[]
  cpu: CpuState
  /** Path of the wear history database, `null` when history is disabled. */
  history_db: string | null
}

/** One row of wear history. */
export interface HistoryPoint {
  ts: number
  device: string
  model: string | null
  serial: string | null
  percentage_used: number | null
  available_spare: number | null
  media_errors: number | null
  power_on_hours: number | null
  power_cycles: number | null
  unsafe_shutdowns: number | null
  data_units_written: number | null
  bytes_written: number | null
  temperature_c: number | null
  critical_warning: number | null
}

export interface HistoryResponse {
  days: number
  since: number
  series: { device: string; points: HistoryPoint[] }[]
}
