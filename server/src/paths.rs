//! Filesystem roots and external tools the daemon uses.
//!
//! Everything is overridable through environment variables so the service can
//! be exercised against a fake sysfs tree (tests, development on a laptop).

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Paths {
    /// `/sys/class/hwmon` — fans and temperature sensors.
    pub hwmon: PathBuf,
    /// `/sys/class/nvme` — NVMe controllers (model, serial, firmware).
    pub nvme: PathBuf,
    /// `/sys/devices/system/cpu` — cpufreq policies, EPP and turbo knobs.
    pub cpu: PathBuf,
    /// `smartctl` binary, the primary source of NVMe wear data.
    pub smartctl: PathBuf,
    /// `nvme` (nvme-cli) binary, an optional fallback for wear data.
    pub nvme_cli: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            hwmon: "/sys/class/hwmon".into(),
            nvme: "/sys/class/nvme".into(),
            cpu: "/sys/devices/system/cpu".into(),
            smartctl: "smartctl".into(),
            nvme_cli: "nvme".into(),
        }
    }
}

impl Paths {
    /// Default paths, with `MS01CTL_SYSFS_*`, `MS01CTL_SMARTCTL` and
    /// `MS01CTL_NVME_CLI` overrides applied.
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            hwmon: env_path("MS01CTL_SYSFS_HWMON", d.hwmon),
            nvme: env_path("MS01CTL_SYSFS_NVME", d.nvme),
            cpu: env_path("MS01CTL_SYSFS_CPU", d.cpu),
            smartctl: env_path("MS01CTL_SMARTCTL", d.smartctl),
            nvme_cli: env_path("MS01CTL_NVME_CLI", d.nvme_cli),
        }
    }
}

fn env_path(key: &str, default: PathBuf) -> PathBuf {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or(default)
}
