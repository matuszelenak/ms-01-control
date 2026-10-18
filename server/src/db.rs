//! SQLite history of NVMe wear telemetry.
//!
//! The database is deliberately tiny and append-only: one row per drive per
//! sample. It is opened with WAL enabled so readers never block the sampler.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::drives::{Drive, WearInfo};

/// Default location; override with the FANCURVE_DB env var (empty disables).
pub const DEFAULT_DB_PATH: &str = "/var/lib/fancurve/history.db";

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS wear_samples (
    id                  INTEGER PRIMARY KEY,
    ts                  INTEGER NOT NULL,
    device              TEXT    NOT NULL,
    model               TEXT,
    serial              TEXT,
    percentage_used     INTEGER,
    available_spare     INTEGER,
    media_errors        INTEGER,
    power_on_hours      INTEGER,
    power_cycles        INTEGER,
    unsafe_shutdowns    INTEGER,
    data_units_written  INTEGER,
    bytes_written       INTEGER,
    temperature_c       INTEGER,
    critical_warning    INTEGER
);
CREATE INDEX IF NOT EXISTS wear_samples_device_ts ON wear_samples (device, ts);
"#;

/// One persisted wear reading.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct WearSample {
    /// Unix seconds.
    pub ts: i64,
    pub device: String,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub percentage_used: Option<i64>,
    pub available_spare: Option<i64>,
    pub media_errors: Option<i64>,
    pub power_on_hours: Option<i64>,
    pub power_cycles: Option<i64>,
    pub unsafe_shutdowns: Option<i64>,
    pub data_units_written: Option<i64>,
    pub bytes_written: Option<i64>,
    pub temperature_c: Option<i64>,
    pub critical_warning: Option<i64>,
}

/// Handle to the history database. Cheap to clone; the connection is shared.
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening history database {}", path.display()))?;
        // WAL keeps a long-running reader from blocking the sampler; both are
        // best-effort (e.g. on filesystems without WAL support).
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        let _ = conn.pragma_update(None, "busy_timeout", 5_000);
        conn.execute_batch(SCHEMA)
            .with_context(|| format!("initialising schema in {}", path.display()))?;
        Ok(Db { conn: Arc::new(Mutex::new(conn)), path: path.to_path_buf() })
    }

    /// Open the database configured through the environment. Returns `None`
    /// when history is disabled or the database cannot be opened — the
    /// service keeps running without history in that case.
    pub fn open_env() -> Option<Db> {
        let path = match std::env::var_os("FANCURVE_DB") {
            Some(v) if v.is_empty() => {
                tracing::info!("FANCURVE_DB is empty: wear history disabled");
                return None;
            }
            Some(v) => PathBuf::from(v),
            None => PathBuf::from(DEFAULT_DB_PATH),
        };
        match Db::open(&path) {
            Ok(db) => {
                tracing::info!("wear history in {}", path.display());
                Some(db)
            }
            Err(e) => {
                tracing::warn!("wear history disabled: {e:#}");
                None
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A panic in another thread poisons the mutex; the connection itself
        // is still usable, so recover instead of propagating the panic.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Append one reading for one drive.
    pub fn insert(&self, ts: i64, drive: &Drive, wear: &WearInfo) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO wear_samples (
                ts, device, model, serial, percentage_used, available_spare,
                media_errors, power_on_hours, power_cycles, unsafe_shutdowns,
                data_units_written, bytes_written, temperature_c, critical_warning
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                ts,
                drive.device,
                drive.model,
                drive.serial,
                wear.percentage_used,
                wear.available_spare,
                wear.media_errors,
                wear.power_on_hours,
                wear.power_cycles,
                wear.unsafe_shutdowns,
                wear.data_units_written,
                wear.bytes_written,
                wear.temperature_c,
                wear.critical_warning,
            ],
        )
        .context("inserting wear sample")?;
        Ok(())
    }

    /// Readings newer than `since`, ordered by device and time and thinned to
    /// at most `max_points` per device (last value in each bucket is kept).
    pub fn history(&self, since: i64, max_points: usize) -> Result<Vec<WearSample>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT ts, device, model, serial, percentage_used, available_spare,
                    media_errors, power_on_hours, power_cycles, unsafe_shutdowns,
                    data_units_written, bytes_written, temperature_c,
                    critical_warning
               FROM wear_samples
              WHERE ts >= ?1
              ORDER BY device, ts",
        )?;
        let rows = stmt.query_map(params![since], row_to_sample)?;
        let all = rows.collect::<Result<Vec<_>, _>>()?;

        let mut out = Vec::new();
        let mut start = 0;
        while start < all.len() {
            let device = &all[start].device;
            let end = all[start..]
                .iter()
                .position(|s| &s.device != device)
                .map_or(all.len(), |p| start + p);
            out.extend(downsample(&all[start..end], max_points));
            start = end;
        }
        Ok(out)
    }

    /// Delete readings older than `before`. Returns the number of rows removed.
    pub fn prune(&self, before: i64) -> Result<usize> {
        let conn = self.lock();
        let removed = conn
            .execute("DELETE FROM wear_samples WHERE ts < ?1", params![before])
            .context("pruning wear history")?;
        Ok(removed)
    }
}

fn row_to_sample(row: &rusqlite::Row<'_>) -> rusqlite::Result<WearSample> {
    Ok(WearSample {
        ts: row.get(0)?,
        device: row.get(1)?,
        model: row.get(2)?,
        serial: row.get(3)?,
        percentage_used: row.get(4)?,
        available_spare: row.get(5)?,
        media_errors: row.get(6)?,
        power_on_hours: row.get(7)?,
        power_cycles: row.get(8)?,
        unsafe_shutdowns: row.get(9)?,
        data_units_written: row.get(10)?,
        bytes_written: row.get(11)?,
        temperature_c: row.get(12)?,
        critical_warning: row.get(13)?,
    })
}

/// Thin `points` (already ordered oldest → newest, one device) to at most
/// `max_points` entries. The oldest and the most recent sample are always
/// kept — they anchor the chart and the "latest value" readout — and the
/// samples in between are thinned to the last reading of each bucket.
pub fn downsample(points: &[WearSample], max_points: usize) -> Vec<WearSample> {
    let max_points = max_points.max(2);
    if points.len() <= max_points {
        return points.to_vec();
    }
    let mut out = Vec::with_capacity(max_points);
    out.push(points[0].clone());

    let rest = &points[1..];
    let buckets = max_points - 1;
    for i in 0..buckets {
        let start = i * rest.len() / buckets;
        let end = ((i + 1) * rest.len() / buckets)
            .max(start + 1)
            .min(rest.len());
        let bucket = &rest[start..end];
        if let Some(last) = bucket.last() {
            if out.last().map(|s| s.ts) != Some(last.ts) {
                out.push(last.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drives::WearInfo;

    fn temp_db(name: &str) -> (Db, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "fancurve-db-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("history.db");
        (Db::open(&path).unwrap(), dir)
    }

    fn drive(device: &str) -> Drive {
        Drive {
            device: device.into(),
            model: Some("Samsung SSD 990 PRO 4TB".into()),
            serial: Some("S7DPNF0Y612074T".into()),
            firmware: Some("4B2QJXD7".into()),
        }
    }

    fn wear(pct: i64, hours: i64) -> WearInfo {
        WearInfo {
            percentage_used: Some(pct),
            available_spare: Some(100),
            media_errors: Some(0),
            power_on_hours: Some(hours),
            data_units_written: Some(1_000),
            bytes_written: Some(1_000 * 512_000),
            temperature_c: Some(37),
            source: "smartctl".into(),
            ..Default::default()
        }
    }

    #[test]
    fn stores_identity_and_derived_fields() {
        let (db, dir) = temp_db("latest");
        db.insert(100, &drive("nvme0"), &wear(3, 10)).unwrap();
        let rows = db.history(0, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].device, "nvme0");
        assert_eq!(rows[0].ts, 100);
        assert_eq!(rows[0].percentage_used, Some(3));
        assert_eq!(rows[0].bytes_written, Some(1_000 * 512_000));
        assert_eq!(rows[0].model.as_deref(), Some("Samsung SSD 990 PRO 4TB"));
        assert_eq!(rows[0].serial.as_deref(), Some("S7DPNF0Y612074T"));
        drop(db);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn history_is_ordered_and_filtered_since() {
        let (db, dir) = temp_db("history");
        for (ts, pct) in [(100, 1), (200, 2), (300, 3), (400, 4)] {
            db.insert(ts, &drive("nvme0"), &wear(pct, 1)).unwrap();
        }
        db.insert(400, &drive("nvme1"), &wear(9, 1)).unwrap();

        let rows = db.history(250, 100).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].device, "nvme0");
        assert_eq!(rows[0].ts, 300);
        assert_eq!(rows[1].ts, 400);
        assert_eq!(rows[2].device, "nvme1");
        drop(db);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn history_is_downsampled_per_device() {
        let (db, dir) = temp_db("downsample");
        for ts in 0..1000 {
            db.insert(ts, &drive("nvme0"), &wear(1, ts)).unwrap();
            db.insert(ts, &drive("nvme1"), &wear(2, ts)).unwrap();
        }
        let rows = db.history(0, 10).unwrap();
        let nvme0: Vec<_> = rows.iter().filter(|r| r.device == "nvme0").collect();
        let nvme1: Vec<_> = rows.iter().filter(|r| r.device == "nvme1").collect();
        assert_eq!(nvme0.len(), 10);
        assert_eq!(nvme1.len(), 10);
        // Newest sample of each device must survive the thinning.
        assert_eq!(nvme0.last().unwrap().ts, 999);
        assert_eq!(nvme1.last().unwrap().ts, 999);
        drop(db);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn downsample_keeps_ends() {
        let points: Vec<WearSample> = (0..100)
            .map(|i| WearSample { ts: i, ..Default::default() })
            .collect();
        let out = downsample(&points, 8);
        assert_eq!(out.len(), 8);
        assert_eq!(out.first().unwrap().ts, 0);
        assert_eq!(out.last().unwrap().ts, 99);
        // Short inputs pass through untouched.
        assert_eq!(downsample(&points[..5], 8).len(), 5);
    }

    #[test]
    fn prunes_old_rows() {
        let (db, dir) = temp_db("prune");
        db.insert(100, &drive("nvme0"), &wear(1, 1)).unwrap();
        db.insert(5000, &drive("nvme0"), &wear(2, 2)).unwrap();
        assert_eq!(db.prune(1000).unwrap(), 1);
        let rows = db.history(0, 100).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ts, 5000);
        drop(db);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reopening_existing_database_keeps_data() {
        let (db, dir) = temp_db("reopen");
        let path = db.path().to_path_buf();
        db.insert(1, &drive("nvme0"), &wear(5, 5)).unwrap();
        drop(db);
        let db = Db::open(&path).unwrap();
        assert_eq!(db.history(0, 10).unwrap().len(), 1);
        drop(db);
        std::fs::remove_dir_all(dir).ok();
    }
}
