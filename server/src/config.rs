//! Fan curve configuration: data model, validation, JSON persistence.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Default config location; override with the FANCURVE_CONFIG env var.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/fancurve/config.json";

/// One point of a fan curve: at `temp` °C run the fan at `pwm` percent.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct CurvePoint {
    pub temp: f64,
    pub pwm: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FanMode {
    /// Firmware automatic control (restores the enable value and Smart Fan IV
    /// points seen at startup).
    Auto,
    /// Software control following the configured curve.
    Curve,
    /// The user's curve programmed into the chip's Smart Fan IV engine; the
    /// hardware runs the loop autonomously (survives daemon/OS crashes).
    Hardware,
    /// Software control at a fixed duty cycle.
    Manual,
}

/// CPU power profile the daemon can enforce.
///
/// The profiles combine the cpufreq governor, the energy-performance
/// preference (EPP) hint and the turbo toggle; see [`CpuProfile::governor`],
/// [`CpuProfile::epp`] and [`CpuProfile::turbo`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CpuProfile {
    /// Maximum performance: performance governor, EPP `performance`, turbo on.
    Performance,
    /// Balanced: powersave governor with an EPP hint towards performance,
    /// turbo on.
    Balanced,
    /// Power efficiency: powersave governor, EPP `power`, turbo off.
    Efficiency,
}

impl CpuProfile {
    pub const ALL: [CpuProfile; 3] = [
        CpuProfile::Performance,
        CpuProfile::Balanced,
        CpuProfile::Efficiency,
    ];

    /// cpufreq governor this profile wants.
    pub fn governor(self) -> &'static str {
        match self {
            CpuProfile::Performance => "performance",
            CpuProfile::Balanced | CpuProfile::Efficiency => "powersave",
        }
    }

    /// Energy-performance preference this profile wants.
    pub fn epp(self) -> &'static str {
        match self {
            CpuProfile::Performance => "performance",
            CpuProfile::Balanced => "balance_performance",
            CpuProfile::Efficiency => "power",
        }
    }

    /// Whether turbo boost should be enabled.
    pub fn turbo(self) -> bool {
        !matches!(self, CpuProfile::Efficiency)
    }
}

/// CPU power-management section of the config.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CpuConfig {
    /// Profile applied at start-up and whenever it is changed in the UI.
    /// `None` means the daemon leaves the CPU settings alone (monitor only).
    #[serde(default)]
    pub profile: Option<CpuProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanConfig {
    pub mode: FanMode,
    /// Duty cycle in percent used in Manual mode.
    pub manual_pwm: f64,
    /// Curve points, kept sorted by temperature.
    pub curve: Vec<CurvePoint>,
    /// Curve used in Hardware mode. Constrained by the Smart Fan IV engine:
    /// a fixed number of points (5 on the nct6798) and non-decreasing duty.
    #[serde(default = "default_hw_curve")]
    pub hw_curve: Vec<CurvePoint>,
}

/// Default hardware curve; also used when loading configs that predate it.
fn default_hw_curve() -> Vec<CurvePoint> {
    vec![
        CurvePoint { temp: 40.0, pwm: 20.0 },
        CurvePoint { temp: 55.0, pwm: 30.0 },
        CurvePoint { temp: 70.0, pwm: 50.0 },
        CurvePoint { temp: 80.0, pwm: 75.0 },
        CurvePoint { temp: 90.0, pwm: 100.0 },
    ]
}

/// Linearly interpolate a curve at `temp`, returning percent (0-100).
/// Clamps to the first/last point outside the curve's range.
pub fn interpolate(pts: &[CurvePoint], temp: f64) -> f64 {
    match pts.iter().position(|p| p.temp >= temp) {
        Some(0) => pts[0].pwm,
        Some(i) => {
            let (a, b) = (pts[i - 1], pts[i]);
            if (b.temp - a.temp).abs() < f64::EPSILON {
                b.pwm
            } else {
                a.pwm + (b.pwm - a.pwm) * (temp - a.temp) / (b.temp - a.temp)
            }
        }
        None => pts.last().map(|p| p.pwm).unwrap_or(100.0),
    }
}

impl FanConfig {
    /// Interpolate the software curve at `temp`.
    pub fn curve_pwm_at(&self, temp: f64) -> f64 {
        interpolate(&self.curve, temp)
    }

    /// The hardware curve resampled to exactly `count` points, as the chip
    /// requires. A matching count passes through unchanged; otherwise points
    /// are sampled evenly across the configured temperature span.
    pub fn hw_points(&self, count: usize) -> Vec<CurvePoint> {
        if self.hw_curve.len() == count || self.hw_curve.is_empty() || count < 2 {
            return self.hw_curve.clone();
        }
        let first = self.hw_curve[0].temp;
        let last = self.hw_curve[self.hw_curve.len() - 1].temp;
        (0..count)
            .map(|i| {
                let temp = first + (last - first) * i as f64 / (count - 1) as f64;
                CurvePoint { temp, pwm: interpolate(&self.hw_curve, temp) }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Control loop interval in milliseconds.
    pub poll_interval_ms: u64,
    /// Per-fan configuration, index 0 = fan1, index 1 = fan2.
    pub fans: Vec<FanConfig>,
    /// CPU power profile management.
    #[serde(default)]
    pub cpu: CpuConfig,
    /// Seconds between NVMe wear samples; 0 disables periodic sampling (the
    /// daemon still samples once at start-up and on manual refresh).
    #[serde(default = "default_wear_interval_secs")]
    pub wear_sample_interval_secs: u64,
    /// Wear history retention in days; 0 keeps everything forever.
    #[serde(default = "default_wear_retention_days")]
    pub wear_retention_days: u32,
}

fn default_wear_interval_secs() -> u64 {
    3600
}

fn default_wear_retention_days() -> u32 {
    1095
}

impl Default for Config {
    fn default() -> Self {
        // Conservative default: idle ~25%, ramps to full by 85°C. The fans
        // start spinning around raw 40 (~16%), so the floor stays above that.
        let curve = vec![
            CurvePoint { temp: 40.0, pwm: 20.0 },
            CurvePoint { temp: 50.0, pwm: 30.0 },
            CurvePoint { temp: 60.0, pwm: 45.0 },
            CurvePoint { temp: 70.0, pwm: 65.0 },
            CurvePoint { temp: 80.0, pwm: 85.0 },
            CurvePoint { temp: 85.0, pwm: 100.0 },
        ];
        Config {
            poll_interval_ms: 2000,
            cpu: CpuConfig::default(),
            wear_sample_interval_secs: default_wear_interval_secs(),
            wear_retention_days: default_wear_retention_days(),
            fans: vec![
                FanConfig {
                    mode: FanMode::Auto,
                    manual_pwm: 50.0,
                    curve: curve.clone(),
                    hw_curve: default_hw_curve(),
                },
                FanConfig {
                    mode: FanMode::Auto,
                    manual_pwm: 50.0,
                    curve,
                    hw_curve: default_hw_curve(),
                },
            ],
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        std::env::var_os("FANCURVE_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH))
    }

    /// Load the config from disk, falling back to defaults (and writing them
    /// out) when the file does not exist yet.
    pub fn load_or_default(path: &Path) -> Result<Config> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let cfg: Config = serde_json::from_str(&text)
                    .with_context(|| format!("parsing {}", path.display()))?;
                cfg.validate()?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                if let Err(e) = cfg.save(path) {
                    tracing::warn!("could not write default config: {e:#}");
                }
                Ok(cfg)
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(self)?;
        // Write atomically so a crash can't leave a truncated config.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if !(250..=60_000).contains(&self.poll_interval_ms) {
            bail!("poll_interval_ms must be between 250 and 60000");
        }
        // 0 disables periodic sampling; otherwise sample at most once a minute.
        if self.wear_sample_interval_secs != 0
            && !(60..=86_400).contains(&self.wear_sample_interval_secs)
        {
            bail!("wear_sample_interval_secs must be 0 or between 60 and 86400");
        }
        if self.wear_retention_days != 0 && !(7..=3650).contains(&self.wear_retention_days) {
            bail!("wear_retention_days must be 0 or between 7 and 3650");
        }
        if self.fans.is_empty() || self.fans.len() > 8 {
            bail!("expected 1-8 fan configs, got {}", self.fans.len());
        }
        for (i, fan) in self.fans.iter().enumerate() {
            if !(0.0..=100.0).contains(&fan.manual_pwm) {
                bail!("fan {}: manual_pwm out of range 0-100", i + 1);
            }
            if fan.curve.len() < 2 {
                bail!("fan {}: curve needs at least 2 points", i + 1);
            }
            for p in &fan.curve {
                if !(0.0..=120.0).contains(&p.temp) || !(0.0..=100.0).contains(&p.pwm) {
                    bail!("fan {}: curve point out of range: {p:?}", i + 1);
                }
            }
            if fan.curve.windows(2).any(|w| w[1].temp < w[0].temp) {
                bail!("fan {}: curve points must be sorted by temperature", i + 1);
            }
            // The Smart Fan IV engine additionally requires non-decreasing
            // duty; point count is matched to the chip in the control loop.
            if !(2..=7).contains(&fan.hw_curve.len()) {
                bail!("fan {}: hardware curve needs 2-7 points", i + 1);
            }
            for p in &fan.hw_curve {
                if !(0.0..=120.0).contains(&p.temp) || !(0.0..=100.0).contains(&p.pwm) {
                    bail!("fan {}: hardware curve point out of range: {p:?}", i + 1);
                }
            }
            if fan
                .hw_curve
                .windows(2)
                .any(|w| w[1].temp < w[0].temp || w[1].pwm < w[0].pwm)
            {
                bail!(
                    "fan {}: hardware curve must have non-decreasing temperatures and duty",
                    i + 1
                );
            }
        }
        Ok(())
    }
}

/// Convert a percent duty cycle to the raw 0-255 PWM value.
pub fn pct_to_raw(pct: f64) -> u8 {
    (pct.clamp(0.0, 100.0) / 100.0 * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fan(points: &[(f64, f64)]) -> FanConfig {
        FanConfig {
            mode: FanMode::Curve,
            manual_pwm: 50.0,
            curve: points
                .iter()
                .map(|&(temp, pwm)| CurvePoint { temp, pwm })
                .collect(),
            hw_curve: default_hw_curve(),
        }
    }

    #[test]
    fn interpolation() {
        let f = fan(&[(40.0, 20.0), (60.0, 40.0), (80.0, 100.0)]);
        assert_eq!(f.curve_pwm_at(30.0), 20.0); // below range clamps
        assert_eq!(f.curve_pwm_at(40.0), 20.0);
        assert_eq!(f.curve_pwm_at(50.0), 30.0); // midpoint
        assert_eq!(f.curve_pwm_at(70.0), 70.0);
        assert_eq!(f.curve_pwm_at(90.0), 100.0); // above range clamps
    }

    #[test]
    fn pct_conversion() {
        assert_eq!(pct_to_raw(0.0), 0);
        assert_eq!(pct_to_raw(100.0), 255);
        assert_eq!(pct_to_raw(50.0), 128);
        assert_eq!(pct_to_raw(-5.0), 0);
        assert_eq!(pct_to_raw(150.0), 255);
    }

    #[test]
    fn default_config_is_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn hw_resampling() {
        let f = fan(&[(0.0, 0.0)]); // software curve irrelevant here
                                    // matching count passes through unchanged
        assert_eq!(f.hw_points(5), f.hw_curve);
        // resampled to 3 points: ends preserved, middle interpolated
        let three = fan(&[(0.0, 0.0)]).hw_points(3);
        assert_eq!(three.len(), 3);
        assert_eq!(three[0], f.hw_curve[0]);
        assert_eq!(three[2], f.hw_curve[4]);
        assert_eq!(three[1].temp, 65.0);
        assert_eq!(three[1].pwm, interpolate(&f.hw_curve, 65.0));
    }

    #[test]
    fn loads_configs_without_the_new_fields() {
        // A config written by 0.2.x has neither the cpu nor the wear settings.
        let json = r#"{
            "poll_interval_ms": 2000,
            "fans": [
                { "mode": "auto", "manual_pwm": 50,
                  "curve": [{ "temp": 40.0, "pwm": 20.0 }, { "temp": 80.0, "pwm": 100.0 }] }
            ]
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.cpu.profile, None);
        assert_eq!(cfg.wear_sample_interval_secs, 3600);
        assert_eq!(cfg.wear_retention_days, 1095);
        cfg.validate().unwrap();
    }

    #[test]
    fn cpu_profile_round_trips_lowercase() {
        assert_eq!(
            serde_json::to_string(&CpuProfile::Performance).unwrap(),
            r#""performance""#
        );
        assert_eq!(
            serde_json::to_string(&CpuProfile::Balanced).unwrap(),
            r#""balanced""#
        );
        assert_eq!(
            serde_json::to_string(&CpuProfile::Efficiency).unwrap(),
            r#""efficiency""#
        );
        let p: CpuProfile = serde_json::from_str(r#""efficiency""#).unwrap();
        assert_eq!(p, CpuProfile::Efficiency);
        let cpu: CpuConfig = serde_json::from_str(r#"{ "profile": null }"#).unwrap();
        assert_eq!(cpu.profile, None);
    }

    #[test]
    fn validates_wear_settings() {
        let cfg = Config {
            wear_sample_interval_secs: 0, // disabled is allowed
            ..Config::default()
        };
        assert!(cfg.validate().is_ok());

        let cfg = Config {
            wear_sample_interval_secs: 5, // too frequent
            ..Config::default()
        };
        assert!(cfg.validate().is_err());

        let cfg = Config { wear_sample_interval_secs: 86_401, ..Config::default() };
        assert!(cfg.validate().is_err());

        let cfg = Config {
            wear_retention_days: 0, // keep everything
            ..Config::default()
        };
        assert!(cfg.validate().is_ok());

        let cfg = Config {
            wear_retention_days: 3, // too short
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn hw_curve_must_be_monotonic() {
        let mut cfg = Config::default();
        cfg.fans[0].hw_curve[1].pwm = 5.0; // duty decreases after point 0
        assert!(cfg.validate().is_err());
    }
}
