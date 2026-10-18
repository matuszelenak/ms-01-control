//! NVMe drive discovery and SMART wear telemetry.
//!
//! Wear data (percentage used, bytes written, power-on hours, spare blocks,
//! media errors) is not exposed through sysfs, so it is read by shelling out
//! to `smartctl -A -j`. `nvme smart-log -o json` (nvme-cli) is used as a
//! fallback when smartctl is missing or fails.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::paths::Paths;

/// An NVMe controller found under `/sys/class/nvme`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drive {
    /// Controller name, e.g. `nvme0`.
    pub device: String,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub firmware: Option<String>,
}

/// SMART wear figures for one drive. Every field is optional: different
/// drives and tools report slightly different subsets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WearInfo {
    /// NVMe "percentage used" — the vendor's endurance estimate, 0-100+.
    pub percentage_used: Option<i64>,
    pub available_spare: Option<i64>,
    pub available_spare_threshold: Option<i64>,
    pub media_errors: Option<i64>,
    pub num_err_log_entries: Option<i64>,
    pub power_on_hours: Option<i64>,
    pub power_cycles: Option<i64>,
    pub unsafe_shutdowns: Option<i64>,
    pub data_units_written: Option<i64>,
    /// `data_units_written * 512_000` (one unit is 1000 512-byte blocks).
    pub bytes_written: Option<i64>,
    pub temperature_c: Option<i64>,
    pub critical_warning: Option<i64>,
    /// Which tool produced the reading: `smartctl` or `nvme-cli`.
    pub source: String,
}

impl WearInfo {
    /// Fill in derived fields. Returns false when the reading carries no
    /// endurance information at all (not an NVMe drive, unsupported device).
    fn finish(mut self) -> Option<WearInfo> {
        self.bytes_written = self.data_units_written.map(|u| u * 512_000);
        if self.percentage_used.is_none()
            && self.data_units_written.is_none()
            && self.available_spare.is_none()
        {
            return None;
        }
        Some(self)
    }
}

/// Enumerate the NVMe controllers under `/sys/class/nvme`.
pub fn discover(paths: &Paths) -> Vec<Drive> {
    let mut drives = Vec::new();
    let Ok(entries) = std::fs::read_dir(&paths.nvme) else {
        return drives;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // The class directory contains one symlink per controller (nvme0, ...).
        if !name.starts_with("nvme") || !name[4..].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let dir = entry.path();
        drives.push(Drive {
            device: name,
            model: read_attr(&dir, "model"),
            serial: read_attr(&dir, "serial"),
            firmware: read_attr(&dir, "firmware_rev"),
        });
    }
    drives.sort_by(|a, b| a.device.cmp(&b.device));
    drives
}

fn read_attr(dir: &Path, attr: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(attr))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read the wear figures of a drive, preferring smartctl.
pub fn read_wear(paths: &Paths, device: &str) -> Result<WearInfo> {
    let dev = format!("/dev/{device}");
    let smartctl = read_smartctl(&paths.smartctl, &dev);
    if let Ok(wear) = smartctl {
        return Ok(wear);
    }
    match read_nvme_cli(&paths.nvme_cli, &dev) {
        Ok(wear) => Ok(wear),
        Err(cli_err) => {
            // Report the smartctl failure first: it is the primary source.
            let smartctl_err = smartctl.unwrap_err();
            bail!("smartctl: {smartctl_err:#}; nvme-cli: {cli_err:#}")
        }
    }
}

fn read_smartctl(bin: &Path, dev: &str) -> Result<WearInfo> {
    let output = Command::new(bin)
        .args(["-A", "-j", dev])
        .output()
        .with_context(|| format!("running {} -A -j {dev}", bin.display()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    if text.trim().is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{} produced no output: {}", bin.display(), stderr.trim());
    }
    let json: Value =
        serde_json::from_str(&text).with_context(|| format!("parsing smartctl JSON for {dev}"))?;
    match parse_smartctl(&json) {
        Some(wear) => Ok(wear),
        None => bail!(
            "{}",
            smartctl_messages(&json)
                .unwrap_or_else(|| "no NVMe SMART data in smartctl output".into())
        ),
    }
}

fn read_nvme_cli(bin: &Path, dev: &str) -> Result<WearInfo> {
    let output = Command::new(bin)
        .args(["smart-log", "--output-format=json", dev])
        .output()
        .with_context(|| format!("running {} smart-log {dev}", bin.display()))?;
    if !output.status.success() && output.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{} failed: {}", bin.display(), stderr.trim());
    }
    let json: Value = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parsing nvme-cli JSON for {dev}"))?;
    parse_nvme_cli(&json).with_context(|| format!("no NVMe SMART data for {dev}"))
}

/// Error messages smartctl embeds in its JSON when it cannot talk to the drive.
fn smartctl_messages(json: &Value) -> Option<String> {
    let messages = json.get("smartctl")?.get("messages")?.as_array()?;
    let joined: Vec<&str> = messages
        .iter()
        .filter_map(|m| m.get("string").and_then(|s| s.as_str()))
        .collect();
    if joined.is_empty() {
        None
    } else {
        Some(joined.join("; "))
    }
}

fn json_i64(value: &Value, key: &str) -> Option<i64> {
    let v = value.get(key)?;
    v.as_i64()
        .or_else(|| v.as_u64().map(|u| u as i64))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// Parse `smartctl -A -j` output.
fn parse_smartctl(json: &Value) -> Option<WearInfo> {
    let mut wear = WearInfo { source: "smartctl".into(), ..Default::default() };

    if let Some(log) = json
        .get("nvme_smart_health_information_log")
        .filter(|l| l.is_object())
    {
        wear.percentage_used = json_i64(log, "percentage_used");
        wear.available_spare = json_i64(log, "available_spare");
        wear.available_spare_threshold = json_i64(log, "available_spare_threshold");
        wear.media_errors = json_i64(log, "media_errors");
        wear.num_err_log_entries = json_i64(log, "num_err_log_entries");
        wear.power_on_hours = json_i64(log, "power_on_hours");
        wear.power_cycles = json_i64(log, "power_cycles");
        wear.unsafe_shutdowns = json_i64(log, "unsafe_shutdowns");
        wear.data_units_written = json_i64(log, "data_units_written");
        wear.temperature_c = json_i64(log, "temperature");
        wear.critical_warning = json_i64(log, "critical_warning");
    }

    // Newer smartctl versions also publish convenience fields at the top
    // level; use them when the log object was absent or partial.
    if wear.percentage_used.is_none() {
        wear.percentage_used = json
            .get("endurance_used")
            .and_then(|v| json_i64(v, "current_percent"));
    }
    if wear.available_spare.is_none() {
        wear.available_spare = json
            .get("spare_available")
            .and_then(|v| json_i64(v, "current_percent"));
        wear.available_spare_threshold = json
            .get("spare_available")
            .and_then(|v| json_i64(v, "threshold_percent"));
    }
    if wear.power_on_hours.is_none() {
        wear.power_on_hours = json.get("power_on_time").and_then(|v| json_i64(v, "hours"));
    }
    if wear.power_cycles.is_none() {
        wear.power_cycles = json_i64(json, "power_cycle_count");
    }
    if wear.temperature_c.is_none() {
        wear.temperature_c = json.get("temperature").and_then(|v| json_i64(v, "current"));
    }

    wear.finish()
}

/// Parse `nvme smart-log --output-format=json`. Note that nvme-cli reports the
/// temperature in kelvin.
fn parse_nvme_cli(json: &Value) -> Option<WearInfo> {
    if !json.is_object() {
        return None;
    }
    let wear = WearInfo {
        percentage_used: json_i64(json, "percent_used")
            .or_else(|| json_i64(json, "percentage_used")),
        available_spare: json_i64(json, "avail_spare")
            .or_else(|| json_i64(json, "available_spare")),
        available_spare_threshold: json_i64(json, "spare_thresh")
            .or_else(|| json_i64(json, "available_spare_threshold")),
        media_errors: json_i64(json, "media_errors"),
        num_err_log_entries: json_i64(json, "num_err_log_entries"),
        power_on_hours: json_i64(json, "power_on_hours"),
        power_cycles: json_i64(json, "power_cycles"),
        unsafe_shutdowns: json_i64(json, "unsafe_shutdowns"),
        data_units_written: json_i64(json, "data_units_written"),
        bytes_written: None,
        temperature_c: json_i64(json, "temperature").map(|k| k - 273),
        critical_warning: json_i64(json, "critical_warning"),
        source: "nvme-cli".into(),
    };
    wear.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real `smartctl -A -j /dev/nvme1` on the MS-01.
    const SMARTCTL: &str = r#"{
      "json_format_version": [1, 0],
      "smartctl": { "version": [7, 5], "exit_status": 0 },
      "device": { "name": "/dev/nvme1", "type": "nvme", "protocol": "NVMe" },
      "temperature": { "current": 37 },
      "spare_available": { "current_percent": 100, "threshold_percent": 10 },
      "endurance_used": { "current_percent": 4 },
      "power_cycle_count": 140,
      "power_on_time": { "hours": 4530 },
      "nvme_smart_health_information_log": {
        "nsid": -1,
        "critical_warning": 0,
        "temperature": 37,
        "available_spare": 100,
        "available_spare_threshold": 10,
        "percentage_used": 4,
        "data_units_read": 162424518,
        "data_units_written": 73476239,
        "host_reads": 1011731324,
        "host_writes": 1913433963,
        "controller_busy_time": 8269,
        "power_cycles": 140,
        "power_on_hours": 4530,
        "unsafe_shutdowns": 30,
        "media_errors": 0,
        "num_err_log_entries": 1,
        "warning_temp_time": 20,
        "critical_comp_time": 9,
        "temperature_sensors": [37, 44]
      }
    }"#;

    #[test]
    fn parses_smartctl() {
        let json: Value = serde_json::from_str(SMARTCTL).unwrap();
        let w = parse_smartctl(&json).expect("wear data");
        assert_eq!(w.source, "smartctl");
        assert_eq!(w.percentage_used, Some(4));
        assert_eq!(w.available_spare, Some(100));
        assert_eq!(w.available_spare_threshold, Some(10));
        assert_eq!(w.power_on_hours, Some(4530));
        assert_eq!(w.power_cycles, Some(140));
        assert_eq!(w.unsafe_shutdowns, Some(30));
        assert_eq!(w.media_errors, Some(0));
        assert_eq!(w.data_units_written, Some(73_476_239));
        // 1000 * 512 bytes per data unit
        assert_eq!(w.bytes_written, Some(73_476_239 * 512_000));
        assert_eq!(w.temperature_c, Some(37));
        assert_eq!(w.critical_warning, Some(0));
    }

    #[test]
    fn parses_smartctl_without_log_object() {
        // Older/other schemas: only the convenience fields are present.
        let json: Value = serde_json::from_str(
            r#"{
              "temperature": { "current": 41 },
              "spare_available": { "current_percent": 97, "threshold_percent": 10 },
              "endurance_used": { "current_percent": 12 },
              "power_on_time": { "hours": 10 },
              "power_cycle_count": 3
            }"#,
        )
        .unwrap();
        let w = parse_smartctl(&json).expect("wear data");
        assert_eq!(w.percentage_used, Some(12));
        assert_eq!(w.available_spare, Some(97));
        assert_eq!(w.temperature_c, Some(41));
        assert_eq!(w.power_on_hours, Some(10));
        assert_eq!(w.power_cycles, Some(3));
    }

    #[test]
    fn rejects_output_without_wear_data() {
        // This is exactly what smartctl prints when it cannot open the device.
        let json: Value = serde_json::from_str(
            r#"{
              "smartctl": {
                "exit_status": 2,
                "messages": [
                  { "string": "Smartctl open device: /dev/nvme0 failed: Permission denied",
                    "severity": "error" }
                ]
              }
            }"#,
        )
        .unwrap();
        assert!(parse_smartctl(&json).is_none());
        assert_eq!(
            smartctl_messages(&json).as_deref(),
            Some("Smartctl open device: /dev/nvme0 failed: Permission denied")
        );
    }

    #[test]
    fn parses_nvme_cli_with_kelvin() {
        let json: Value = serde_json::from_str(
            r#"{
              "critical_warning": 0,
              "temperature": 313,
              "avail_spare": 100,
              "spare_thresh": 10,
              "percent_used": 7,
              "data_units_read": 10,
              "data_units_written": 2048,
              "power_cycles": 12,
              "power_on_hours": 99,
              "unsafe_shutdowns": 2,
              "media_errors": 1,
              "num_err_log_entries": 3
            }"#,
        )
        .unwrap();
        let w = parse_nvme_cli(&json).expect("wear data");
        assert_eq!(w.source, "nvme-cli");
        assert_eq!(w.percentage_used, Some(7));
        assert_eq!(w.available_spare, Some(100));
        assert_eq!(w.available_spare_threshold, Some(10));
        assert_eq!(w.temperature_c, Some(40)); // 313 K
        assert_eq!(w.data_units_written, Some(2048));
        assert_eq!(w.bytes_written, Some(2048 * 512_000));
        assert_eq!(w.media_errors, Some(1));
    }

    #[test]
    fn discovers_controllers_from_fake_sysfs() {
        let root = std::env::temp_dir().join(format!("ms01ctl-nvme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (dev, model, serial) in [
            ("nvme0", "Force MP510", "204782480001291711F6"),
            ("nvme1", "Samsung SSD 990 PRO 4TB", "S7DPNF0Y612074T"),
        ] {
            let dir = root.join(dev);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("model"), format!("{model}        \n")).unwrap();
            std::fs::write(dir.join("serial"), format!("{serial}     \n")).unwrap();
            std::fs::write(dir.join("firmware_rev"), "4B2QJXD7\n").unwrap();
        }
        // Noise: non-controller entries must be ignored.
        std::fs::create_dir_all(root.join("nvme0n1")).unwrap();
        std::fs::write(root.join("uevent"), "").unwrap();

        let paths = Paths { nvme: root.clone(), ..Default::default() };
        let drives = discover(&paths);
        assert_eq!(drives.len(), 2);
        assert_eq!(drives[0].device, "nvme0");
        assert_eq!(drives[0].model.as_deref(), Some("Force MP510"));
        assert_eq!(drives[1].serial.as_deref(), Some("S7DPNF0Y612074T"));
        assert_eq!(drives[1].firmware.as_deref(), Some("4B2QJXD7"));

        std::fs::remove_dir_all(&root).ok();
    }
}
