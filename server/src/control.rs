//! Background loops: the fan control loop, and the NVMe wear sampler.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::{Mutex, RwLock};

use crate::config::{pct_to_raw, Config, CurvePoint, FanMode};
use crate::cpu::{CpuControl, CpuState};
use crate::db::Db;
use crate::drives::{self, Drive, WearInfo};
use crate::hwmon::Hardware;
use crate::paths::Paths;

#[derive(Debug, Clone, Serialize, Default)]
pub struct SensorStatus {
    pub label: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub control: bool,
    pub temp: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct FanStatus {
    pub index: u32,
    pub rpm: Option<u32>,
    pub pwm_raw: Option<u32>,
    pub pwm_pct: Option<f64>,
    pub enable: Option<u32>,
    pub mode: Option<FanMode>,
    /// The duty the controller is targeting (None in auto/hardware modes,
    /// where the chip runs the loop itself).
    pub target_pct: Option<f64>,
    /// How many Smart Fan IV curve points the chip supports for this fan.
    pub hw_points: usize,
}

/// One NVMe drive as shown in the UI: identity plus the latest wear reading.
#[derive(Debug, Clone, Serialize, Default)]
pub struct DriveStatus {
    pub device: String,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub firmware: Option<String>,
    /// Latest SMART wear figures (`None` when they could not be read).
    pub wear: Option<WearInfo>,
    /// Why the wear data is missing, if it is.
    pub wear_error: Option<String>,
    /// Unix seconds of the wear reading.
    pub sampled_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Status {
    pub sensors: Vec<SensorStatus>,
    /// max() over the control sensors (CPU package) — the temperature the
    /// curves are evaluated at.
    pub control_temp: Option<f64>,
    pub fans: Vec<FanStatus>,
    pub drives: Vec<DriveStatus>,
    pub cpu: CpuState,
    /// Path of the wear history database, when history is enabled.
    pub history_db: Option<String>,
}

/// Latest wear reading of one drive.
#[derive(Debug, Clone, Default)]
pub struct DriveWear {
    pub device: String,
    pub wear: Option<WearInfo>,
    pub error: Option<String>,
}

/// Snapshot of the most recent wear readings, shared with the API.
#[derive(Debug, Clone, Default)]
pub struct WearSnapshot {
    pub sampled_at: Option<i64>,
    pub drives: Vec<DriveWear>,
}

pub struct AppState {
    pub hw: Hardware,
    pub config: RwLock<Config>,
    pub status: RwLock<Status>,
    pub paths: Paths,
    pub cpu: CpuControl,
    /// NVMe controllers found at start-up.
    pub drives: Vec<Drive>,
    pub wear: RwLock<WearSnapshot>,
    /// Wear history database; `None` when history is disabled or unavailable.
    pub db: Option<Db>,
    /// Serialises SMART reads between the sampler and manual refreshes.
    pub sample_lock: Mutex<()>,
}

pub type SharedState = Arc<AppState>;

/// Seconds since the unix epoch.
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Per-fan bookkeeping so we only touch sysfs when something changes.
#[derive(Default, Clone)]
struct FanRuntime {
    last_mode: Option<FanMode>,
    last_raw: Option<u8>,
    /// The hardware curve last programmed into the chip.
    last_hw_curve: Option<Vec<CurvePoint>>,
}

/// The fan control loop.
pub async fn run(state: SharedState) {
    let mut runtimes = vec![FanRuntime::default(); state.hw.fans.len()];

    loop {
        let interval_ms = {
            let config = state.config.read().await;
            tick(&state, &config, &mut runtimes).await;
            config.poll_interval_ms
        };
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
    }
}

/// Assemble the drive list shown in the UI from the discovered controllers
/// and the latest wear snapshot.
pub async fn drive_statuses(state: &SharedState) -> Vec<DriveStatus> {
    let wear = state.wear.read().await.clone();
    state
        .drives
        .iter()
        .map(|drive| {
            let reading = wear.drives.iter().find(|w| w.device == drive.device);
            DriveStatus {
                device: drive.device.clone(),
                model: drive.model.clone(),
                serial: drive.serial.clone(),
                firmware: drive.firmware.clone(),
                wear: reading.and_then(|w| w.wear.clone()),
                wear_error: reading.and_then(|w| w.error.clone()),
                sampled_at: wear.sampled_at,
            }
        })
        .collect()
}

/// The wear figures that are worth a new history row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnduranceSignature {
    percentage_used: Option<i64>,
    data_units_written: Option<i64>,
    available_spare: Option<i64>,
    media_errors: Option<i64>,
    critical_warning: Option<i64>,
}

fn endurance_signature(wear: &WearInfo) -> EnduranceSignature {
    EnduranceSignature {
        percentage_used: wear.percentage_used,
        data_units_written: wear.data_units_written,
        available_spare: wear.available_spare,
        media_errors: wear.media_errors,
        critical_warning: wear.critical_warning,
    }
}

/// Read SMART data for every drive, update the in-memory snapshot and append
/// the readings to the history database.
///
/// SMART access blocks, so the reads run on the blocking thread pool. A
/// failure to read one drive never stops the others.
pub async fn sample_wear(state: &SharedState) {
    let _guard = state.sample_lock.lock().await;

    let paths = state.paths.clone();
    let drives = state.drives.clone();
    let results = tokio::task::spawn_blocking(move || {
        drives
            .into_iter()
            .map(|drive| {
                let wear = drives::read_wear(&paths, &drive.device);
                (drive, wear)
            })
            .collect::<Vec<_>>()
    })
    .await;

    let ts = now_secs();
    let results = match results {
        Ok(results) => results,
        Err(e) => {
            tracing::error!("wear sampling task failed: {e}");
            return;
        }
    };

    // Endurance of the previous reading, used to skip unchanged samples.
    let previous: Vec<(String, EnduranceSignature)> = {
        let snapshot = state.wear.read().await;
        snapshot
            .drives
            .iter()
            .filter_map(|w| {
                w.wear
                    .as_ref()
                    .map(|wear| (w.device.clone(), endurance_signature(wear)))
            })
            .collect()
    };

    let mut snapshot = WearSnapshot {
        sampled_at: Some(ts),
        drives: Vec::with_capacity(results.len()),
    };
    for (drive, result) in results {
        match result {
            Ok(wear) => {
                if let Some(db) = &state.db {
                    let last = previous
                        .iter()
                        .find(|(device, _)| *device == drive.device)
                        .map(|(_, signature)| *signature);
                    // Only store a row when the endurance figures moved:
                    // temperature and power-on time change all the time and
                    // would otherwise flood the table on manual refreshes.
                    if last != Some(endurance_signature(&wear)) {
                        if let Err(e) = db.insert(ts, &drive, &wear) {
                            tracing::warn!("storing wear sample of {}: {e:#}", drive.device);
                        }
                    }
                }
                tracing::debug!(
                    "{}: {}% used, {} h powered on",
                    drive.device,
                    wear.percentage_used.unwrap_or(-1),
                    wear.power_on_hours.unwrap_or(-1)
                );
                snapshot.drives.push(DriveWear {
                    device: drive.device,
                    wear: Some(wear),
                    error: None,
                });
            }
            Err(e) => {
                tracing::warn!("reading wear of {}: {e:#}", drive.device);
                snapshot.drives.push(DriveWear {
                    device: drive.device,
                    wear: None,
                    error: Some(format!("{e:#}")),
                });
            }
        }
    }
    *state.wear.write().await = snapshot;
}

/// Periodic wear sampler. Wear counters move very slowly, so the default is
/// hourly; the loop wakes every minute so config changes apply promptly.
pub async fn run_wear_sampler(state: SharedState) {
    // Sample once at start-up so the UI has data without waiting an hour.
    sample_wear(&state).await;
    let mut last_sample = now_secs();
    let mut last_prune = 0i64;

    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let (interval, retention_days) = {
            let config = state.config.read().await;
            (config.wear_sample_interval_secs, config.wear_retention_days)
        };

        if interval > 0 && now_secs() - last_sample >= interval as i64 {
            sample_wear(&state).await;
            last_sample = now_secs();
        }

        // Prune once a day (and once shortly after start-up).
        if now_secs() - last_prune >= 86_400 {
            last_prune = now_secs();
            if retention_days > 0 {
                if let Some(db) = &state.db {
                    let cutoff = now_secs() - retention_days as i64 * 86_400;
                    match db.prune(cutoff) {
                        Ok(0) => {}
                        Ok(n) => tracing::info!(
                            "pruned {n} wear samples older than {retention_days} days"
                        ),
                        Err(e) => tracing::warn!("pruning wear history: {e:#}"),
                    }
                }
            }
        }
    }
}

async fn tick(state: &SharedState, config: &Config, runtimes: &mut [FanRuntime]) {
    // --- read temperatures ---
    let sensors: Vec<SensorStatus> = state
        .hw
        .sensors
        .iter()
        .map(|s| SensorStatus {
            label: s.label.clone(),
            kind: s.kind.to_string(),
            model: s.model.clone(),
            control: s.control,
            temp: s.read().map_err(|e| tracing::warn!("{e:#}")).ok(),
        })
        .collect();
    let control_temp = sensors
        .iter()
        .filter(|s| s.control)
        .filter_map(|s| s.temp)
        .fold(None, |acc: Option<f64>, t| {
            Some(acc.map_or(t, |a| a.max(t)))
        });

    // --- drive fans ---
    let mut fan_statuses = Vec::with_capacity(state.hw.fans.len());
    for (i, fan) in state.hw.fans.iter().enumerate() {
        let fan_cfg = config.fans.get(i);
        let mode = fan_cfg.map(|c| c.mode);
        let rt = &mut runtimes[i];

        let target_pct = match (fan_cfg, mode) {
            (Some(_), Some(FanMode::Auto | FanMode::Hardware)) | (None, _) => None,
            (Some(cfg), Some(FanMode::Manual)) => Some(cfg.manual_pwm),
            (Some(cfg), Some(FanMode::Curve)) => match control_temp {
                Some(t) => Some(cfg.curve_pwm_at(t)),
                // Failsafe: no readable temperature -> full speed.
                None => {
                    tracing::error!("no temperature available, fan {} to 100%", fan.index);
                    Some(100.0)
                }
            },
            (Some(_), None) => None,
        };

        // Apply mode transitions only when they change. Hardware mode is
        // handled below since it must also react to curve edits.
        if mode != rt.last_mode {
            let result = match mode {
                Some(FanMode::Auto) | None => fan.set_auto(),
                Some(FanMode::Hardware) => Ok(()),
                Some(_) => fan.set_manual(),
            };
            match result {
                Ok(()) => {
                    tracing::info!("fan {}: mode -> {:?}", fan.index, mode);
                    rt.last_mode = mode;
                    rt.last_raw = None;
                    rt.last_hw_curve = None;
                }
                Err(e) => tracing::error!("fan {}: mode change failed: {e:#}", fan.index),
            }
        }

        // Program the chip's Smart Fan IV engine when entering hardware mode
        // or whenever the hardware curve changes.
        if let (Some(cfg), Some(FanMode::Hardware)) = (fan_cfg, mode) {
            if rt.last_hw_curve.as_ref() != Some(&cfg.hw_curve) {
                let points: Vec<(i64, u8)> = cfg
                    .hw_points(fan.auto_point_count())
                    .iter()
                    .map(|p| ((p.temp * 1000.0).round() as i64, pct_to_raw(p.pwm)))
                    .collect();
                match fan.set_hardware_curve(&points) {
                    Ok(()) => {
                        tracing::info!("fan {}: hardware curve programmed: {points:?}", fan.index);
                        rt.last_hw_curve = Some(cfg.hw_curve.clone());
                    }
                    Err(e) => {
                        tracing::error!("fan {}: hardware curve failed: {e:#}", fan.index)
                    }
                }
            }
        }

        if let Some(pct) = target_pct {
            let raw = pct_to_raw(pct);
            if rt.last_raw != Some(raw) {
                match fan.set_pwm_raw(raw) {
                    Ok(()) => rt.last_raw = Some(raw),
                    Err(e) => tracing::error!("fan {}: pwm write failed: {e:#}", fan.index),
                }
            }
        }

        let pwm_raw = fan.pwm_raw().ok();
        fan_statuses.push(FanStatus {
            index: fan.index,
            rpm: fan.rpm().ok(),
            pwm_raw,
            pwm_pct: pwm_raw.map(|v| (v as f64 / 255.0 * 1000.0).round() / 10.0),
            enable: fan.enable_value().ok(),
            mode,
            target_pct,
            hw_points: fan.auto_point_count(),
        });
    }

    let drives = drive_statuses(state).await;
    let cpu = state.cpu.state(config.cpu.profile);

    *state.status.write().await = Status {
        sensors,
        control_temp,
        fans: fan_statuses,
        drives,
        cpu,
        history_db: state.db.as_ref().map(|db| db.path().display().to_string()),
    };
}
