//! CPU power-profile control.
//!
//! The daemon switches the host between performance- and efficiency-oriented
//! settings by writing the kernel's cpufreq knobs:
//!
//! - `energy_performance_preference` (EPP) of every policy — the hardware
//!   hint that matters on `intel_pstate`/`amd_pstate` in active mode,
//! - `scaling_governor` of every policy,
//! - the turbo toggle (`intel_pstate/no_turbo`, or `cpufreq/boost`).
//!
//! Every write is best-effort: heterogeneous CPUs, offline cores or a kernel
//! that does not expose a knob produce warnings, never a failure of the whole
//! operation (unless nothing at all could be written).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Result};
use serde::Serialize;

use crate::config::CpuProfile;
use crate::paths::Paths;

/// How the platform exposes the turbo switch.
#[derive(Debug, Clone, Copy)]
enum TurboKind {
    /// `intel_pstate/no_turbo`: 1 disables turbo.
    NoTurbo,
    /// `cpufreq/boost`: 0 disables turbo (acpi-cpufreq and friends).
    Boost,
}

/// A readable/writable turbo control.
#[derive(Debug)]
struct TurboKnob {
    kind: TurboKind,
    path: PathBuf,
    /// Set once the kernel refused a write with EPERM: the firmware disabled
    /// turbo at reset (`IA32_MISC_ENABLE` bit 38 — on the MS-01 the BIOS
    /// "Turbo Mode" / boot performance mode setting) and `intel_pstate`
    /// refuses to flip it. Only the BIOS can undo that, so further attempts
    /// are pointless and must not turn into a warning on every profile switch.
    locked: AtomicBool,
}

impl TurboKnob {
    fn new(kind: TurboKind, path: PathBuf) -> Self {
        Self { kind, path, locked: AtomicBool::new(false) }
    }

    fn read(&self) -> Option<bool> {
        let value: i64 = read_trimmed(&self.path)?.parse().ok()?;
        Some(match self.kind {
            TurboKind::NoTurbo => value == 0,
            TurboKind::Boost => value != 0,
        })
    }

    /// `io::Error` kind is preserved so callers can tell a firmware lock
    /// (EPERM) from a knob the driver rejected for other reasons.
    fn write(&self, enabled: bool) -> std::io::Result<()> {
        let value = match self.kind {
            TurboKind::NoTurbo => i32::from(!enabled),
            TurboKind::Boost => i32::from(enabled),
        };
        fs::write(&self.path, value.to_string()).map_err(|e| {
            std::io::Error::new(e.kind(), format!("writing {}: {e}", self.path.display()))
        })
    }

    fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }

    fn lock(&self) {
        self.locked.store(true, Ordering::Relaxed);
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

/// Live CPU power-management state, as reported to the UI.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CpuState {
    /// Configured profile (`null` = the daemon does not manage the CPU).
    pub profile: Option<CpuProfile>,
    /// Profile matching the live settings, when they match one exactly.
    pub effective: Option<CpuProfile>,
    pub driver: Option<String>,
    pub governor: Option<String>,
    pub epp: Option<String>,
    /// `true` = turbo enabled. `null` = the platform exposes no turbo knob.
    pub turbo: Option<bool>,
    /// `true` = turbo is off and the firmware refuses to change it (BIOS
    /// setting, EPERM on write). The profiles then only switch governor and
    /// EPP, which still makes them "effective".
    pub turbo_locked: bool,
    /// Highest frequency reachable right now, in MHz. With turbo disabled by
    /// the firmware this is the base clock, which is the most direct way to
    /// see that the turbo toggle has no effect.
    pub max_freq_mhz: Option<u32>,
    pub available_governors: Vec<String>,
    pub available_epp: Vec<String>,
    /// Profiles this CPU can take.
    pub supported_profiles: Vec<CpuProfile>,
    /// Number of cpufreq policies found (logical CPUs).
    pub policy_count: usize,
}

impl CpuProfile {
    /// Does a live CPU state correspond to this profile? The turbo toggle is
    /// ignored when the platform does not expose one, or when the firmware
    /// has locked it off (nothing the daemon can do about it).
    pub fn matches(self, state: &CpuState) -> bool {
        let governor_ok = state.governor.as_deref() == Some(self.governor());
        let epp_ok = match state.epp.as_deref() {
            Some(epp) => epp == self.epp(),
            None => true,
        };
        let turbo_ok = if state.turbo_locked {
            true
        } else {
            match state.turbo {
                Some(turbo) => turbo == self.turbo(),
                None => true,
            }
        };
        governor_ok && epp_ok && turbo_ok
    }
}

/// CPU control handle: one cpufreq policy directory per logical CPU plus the
/// turbo knob, if present.
pub struct CpuControl {
    policies: Vec<PathBuf>,
    turbo: Option<TurboKnob>,
}

impl CpuControl {
    pub fn discover(paths: &Paths) -> CpuControl {
        let mut policies = Vec::new();
        if let Ok(entries) = fs::read_dir(&paths.cpu) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_cpu = name
                    .strip_prefix("cpu")
                    .map(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                    .unwrap_or(false);
                if !is_cpu {
                    continue;
                }
                let dir = entry.path().join("cpufreq");
                if dir.join("scaling_governor").exists() {
                    policies.push(dir);
                }
            }
        }
        policies.sort();

        // intel_pstate is the common case on the MS-01; acpi-cpufreq exposes
        // boost through the first policy instead.
        let no_turbo = paths.cpu.join("intel_pstate/no_turbo");
        let boost = policies
            .first()
            .map(|p| p.join("boost"))
            .filter(|p| p.exists());
        let turbo = if no_turbo.exists() {
            Some(TurboKnob::new(TurboKind::NoTurbo, no_turbo))
        } else {
            boost.map(|p| TurboKnob::new(TurboKind::Boost, p))
        };

        if policies.is_empty() {
            tracing::warn!("no cpufreq policies found: CPU profile control unavailable");
        } else {
            tracing::info!(
                "cpu control: {} cpufreq policies, turbo knob {}",
                policies.len(),
                turbo
                    .as_ref()
                    .map(|t| t.path().display().to_string())
                    .unwrap_or_else(|| "none".into())
            );
        }

        CpuControl { policies, turbo }
    }

    /// Read the current settings (from the first policy, they are uniform on
    /// every machine we care about).
    pub fn state(&self, configured: Option<CpuProfile>) -> CpuState {
        let first = self.policies.first();
        let driver = first.and_then(|p| read_trimmed(&p.join("scaling_driver")));
        let governor = first.and_then(|p| read_trimmed(&p.join("scaling_governor")));
        let epp = first.and_then(|p| read_trimmed(&p.join("energy_performance_preference")));
        let available_governors = first
            .map(|p| read_list(&p.join("scaling_available_governors")))
            .unwrap_or_default();
        let available_epp = first
            .map(|p| read_list(&p.join("energy_performance_available_preferences")))
            .unwrap_or_default();
        let turbo = self.turbo.as_ref().and_then(|t| t.read());
        let turbo_locked = self.turbo.as_ref().is_some_and(TurboKnob::is_locked);
        let max_freq_mhz = first
            .and_then(|p| read_trimmed(&p.join("cpuinfo_max_freq")))
            .and_then(|khz| khz.parse::<u64>().ok())
            .map(|khz| (khz / 1000) as u32);

        let mut state = CpuState {
            profile: configured,
            effective: None,
            driver,
            governor,
            epp,
            turbo,
            turbo_locked,
            max_freq_mhz,
            available_governors,
            available_epp,
            supported_profiles: Vec::new(),
            policy_count: self.policies.len(),
        };
        state.effective = CpuProfile::ALL.into_iter().find(|p| p.matches(&state));
        state.supported_profiles = CpuProfile::ALL
            .into_iter()
            .filter(|p| self.supports(*p, &state))
            .collect();
        state
    }

    fn supports(&self, profile: CpuProfile, state: &CpuState) -> bool {
        if self.policies.is_empty() {
            return false;
        }
        let governor_ok = state.available_governors.is_empty()
            || state
                .available_governors
                .iter()
                .any(|g| g == profile.governor());
        let epp_ok = state.available_epp.is_empty()
            || state.available_epp.iter().any(|e| e == profile.epp());
        governor_ok && epp_ok
    }

    /// Can this CPU take `profile`?
    pub fn supports_profile(&self, profile: CpuProfile) -> bool {
        let state = self.state(None);
        self.supports(profile, &state)
    }

    /// Apply `profile` to every policy and the turbo knob. Returns human
    /// readable warnings for anything that could not be set.
    pub fn apply(&self, profile: CpuProfile) -> Result<Vec<String>> {
        if self.policies.is_empty() {
            bail!("no cpufreq interface: cannot change the CPU power profile");
        }

        let mut warnings: BTreeSet<String> = BTreeSet::new();
        let mut applied = 0usize;

        for dir in &self.policies {
            // Governor first: with intel_pstate the governor decides whether
            // the EPP hint is honoured at all.
            let gov_path = dir.join("scaling_governor");
            let gov_available = read_list(&dir.join("scaling_available_governors"));
            if gov_available.is_empty() || gov_available.iter().any(|g| g == profile.governor()) {
                match fs::write(&gov_path, profile.governor()) {
                    Ok(()) => applied += 1,
                    Err(e) => {
                        warnings.insert(format!(
                            "governor '{}' not accepted by {}: {e}",
                            profile.governor(),
                            gov_path.display()
                        ));
                    }
                }
            } else {
                warnings.insert(format!(
                    "governor '{}' unsupported (available: {})",
                    profile.governor(),
                    gov_available.join(", ")
                ));
            }

            let epp_path = dir.join("energy_performance_preference");
            if epp_path.exists() {
                let epp_available =
                    read_list(&dir.join("energy_performance_available_preferences"));
                if epp_available.is_empty() || epp_available.iter().any(|e| e == profile.epp()) {
                    match fs::write(&epp_path, profile.epp()) {
                        Ok(()) => applied += 1,
                        Err(e) => {
                            warnings.insert(format!(
                                "EPP '{}' not accepted by {}: {e}",
                                profile.epp(),
                                epp_path.display()
                            ));
                        }
                    }
                } else {
                    warnings.insert(format!(
                        "EPP '{}' unsupported (available: {})",
                        profile.epp(),
                        epp_available.join(", ")
                    ));
                }
            }
        }

        if let Some(turbo) = &self.turbo {
            let want = profile.turbo();
            let verb = if want { "enable" } else { "disable" };
            if turbo.read() == Some(want) {
                // Already in the desired state. This is also what firmware-
                // locked turbo looks like when the profile wants it off.
            } else if turbo.is_locked() {
                // Known to be firmware-locked: reported through
                // `CpuState::turbo_locked`, not as a warning on every switch.
            } else if let Err(e) = turbo.write(want) {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    turbo.lock();
                    warnings.insert(format!(
                        "turbo {verb}: the firmware disabled turbo and only the BIOS can \
                         re-enable it (MS-01: Advanced → CPU - Power Management Control → \
                         Turbo Mode, then reboot); governor and EPP were applied"
                    ));
                } else {
                    warnings.insert(format!("turbo {verb}: {e}"));
                }
            }
        } else if profile.turbo() {
            warnings.insert("no turbo knob exposed by this kernel".into());
        }

        if applied == 0 {
            bail!(
                "could not write any cpufreq setting: {}",
                warnings.iter().cloned().collect::<Vec<_>>().join("; ")
            );
        }
        tracing::info!(
            "cpu profile -> {:?} (governor {}, epp {}, turbo {})",
            profile,
            profile.governor(),
            profile.epp(),
            profile.turbo()
        );
        Ok(warnings.into_iter().collect())
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn read_list(path: &Path) -> Vec<String> {
    read_trimmed(path)
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_mapping_is_consistent() {
        let perf = CpuProfile::Performance;
        assert_eq!(perf.governor(), "performance");
        assert_eq!(perf.epp(), "performance");
        assert!(perf.turbo());

        let balanced = CpuProfile::Balanced;
        assert_eq!(balanced.governor(), "powersave");
        assert_eq!(balanced.epp(), "balance_performance");
        assert!(balanced.turbo());

        let eco = CpuProfile::Efficiency;
        assert_eq!(eco.governor(), "powersave");
        assert_eq!(eco.epp(), "power");
        assert!(!eco.turbo());
    }

    #[test]
    fn effective_profile_detection() {
        let state = CpuState {
            governor: Some("powersave".into()),
            epp: Some("power".into()),
            turbo: Some(false),
            ..Default::default()
        };
        assert_eq!(
            CpuProfile::ALL.into_iter().find(|p| p.matches(&state)),
            Some(CpuProfile::Efficiency)
        );

        // Stock Proxmox setting: EPP balance_power is not one of our profiles.
        let state = CpuState {
            governor: Some("powersave".into()),
            epp: Some("balance_power".into()),
            turbo: Some(false),
            ..Default::default()
        };
        assert_eq!(
            CpuProfile::ALL.into_iter().find(|p| p.matches(&state)),
            None
        );

        // No turbo knob: turbo does not disqualify a profile.
        let state = CpuState {
            governor: Some("performance".into()),
            epp: Some("performance".into()),
            turbo: None,
            ..Default::default()
        };
        assert_eq!(
            CpuProfile::ALL.into_iter().find(|p| p.matches(&state)),
            Some(CpuProfile::Performance)
        );

        // Turbo locked off by the firmware: governor/EPP still decide, so a
        // profile reads as effective instead of a permanent mismatch.
        let state = CpuState {
            governor: Some("performance".into()),
            epp: Some("performance".into()),
            turbo: Some(false),
            turbo_locked: true,
            ..Default::default()
        };
        assert_eq!(
            CpuProfile::ALL.into_iter().find(|p| p.matches(&state)),
            Some(CpuProfile::Performance)
        );
    }

    /// One policy, an intel_pstate no_turbo knob and a base-clock
    /// cpuinfo_max_freq — the MS-01 with turbo disabled in the BIOS.
    fn locked_turbo_tree(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ms01ctl-cpu-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("cpu0/cpufreq");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("scaling_driver"), "intel_pstate\n").unwrap();
        fs::write(dir.join("scaling_governor"), "powersave\n").unwrap();
        fs::write(dir.join("scaling_available_governors"), "performance powersave\n").unwrap();
        fs::write(dir.join("energy_performance_preference"), "balance_power\n").unwrap();
        fs::write(
            dir.join("energy_performance_available_preferences"),
            "default performance balance_performance balance_power power\n",
        )
        .unwrap();
        fs::write(dir.join("cpuinfo_max_freq"), "2500000\n").unwrap();
        fs::create_dir_all(root.join("intel_pstate")).unwrap();
        fs::write(root.join("intel_pstate/no_turbo"), "1\n").unwrap();
        root
    }

    #[test]
    fn firmware_locked_turbo_goes_quiet_after_the_first_warning() {
        let root = locked_turbo_tree("locked");
        let paths = Paths { cpu: root.clone(), ..Default::default() };
        let cpu = CpuControl::discover(&paths);

        // Simulate the kernel refusing the turbo write with EPERM.
        let turbo = cpu.turbo.as_ref().unwrap();
        turbo.lock();

        let warnings = cpu.apply(CpuProfile::Performance).unwrap();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

        let state = cpu.state(Some(CpuProfile::Performance));
        assert!(state.turbo_locked);
        assert_eq!(state.turbo, Some(false));
        assert_eq!(state.max_freq_mhz, Some(2500));
        // Governor and EPP were applied, so the profile is effective.
        assert_eq!(state.effective, Some(CpuProfile::Performance));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn other_turbo_write_failures_still_warn() {
        // A no_turbo that cannot be read or written at all (here: a
        // directory) is not a firmware lock and must not be masked.
        let root = locked_turbo_tree("broken");
        fs::remove_file(root.join("intel_pstate/no_turbo")).unwrap();
        fs::create_dir_all(root.join("intel_pstate/no_turbo")).unwrap();

        let paths = Paths { cpu: root.clone(), ..Default::default() };
        let cpu = CpuControl::discover(&paths);
        let warnings = cpu.apply(CpuProfile::Performance).unwrap();
        assert!(
            warnings.iter().any(|w| w.starts_with("turbo enable")),
            "expected a turbo warning, got {warnings:?}"
        );
        assert!(!cpu.state(None).turbo_locked);

        fs::remove_dir_all(&root).ok();
    }

    /// Build a fake cpu tree with two policies plus an intel_pstate no_turbo
    /// knob and verify apply() writes the expected values.
    #[test]
    fn applies_profile_to_fake_sysfs() {
        let root = std::env::temp_dir().join(format!("ms01ctl-cpu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for cpu in [0, 1] {
            let dir = root.join(format!("cpu{cpu}/cpufreq"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("scaling_driver"), "intel_pstate\n").unwrap();
            fs::write(dir.join("scaling_governor"), "powersave\n").unwrap();
            fs::write(
                dir.join("scaling_available_governors"),
                "performance powersave\n",
            )
            .unwrap();
            fs::write(dir.join("energy_performance_preference"), "balance_power\n").unwrap();
            fs::write(
                dir.join("energy_performance_available_preferences"),
                "default performance balance_performance balance_power power\n",
            )
            .unwrap();
        }
        fs::create_dir_all(root.join("intel_pstate")).unwrap();
        fs::write(root.join("intel_pstate/no_turbo"), "1\n").unwrap();
        // Non-CPU noise.
        fs::create_dir_all(root.join("cpuidle")).unwrap();

        let paths = Paths { cpu: root.clone(), ..Default::default() };
        let cpu = CpuControl::discover(&paths);
        let state = cpu.state(None);
        assert_eq!(state.policy_count, 2);
        assert_eq!(state.driver.as_deref(), Some("intel_pstate"));
        assert_eq!(state.governor.as_deref(), Some("powersave"));
        assert_eq!(state.epp.as_deref(), Some("balance_power"));
        assert_eq!(state.turbo, Some(false));
        assert_eq!(state.supported_profiles.len(), 3);
        // Stock EPP is not one of our profiles.
        assert_eq!(state.effective, None);

        let warnings = cpu.apply(CpuProfile::Performance).unwrap();
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            fs::read_to_string(root.join("cpu0/cpufreq/scaling_governor")).unwrap(),
            "performance"
        );
        assert_eq!(
            fs::read_to_string(root.join("cpu1/cpufreq/energy_performance_preference")).unwrap(),
            "performance"
        );
        assert_eq!(
            fs::read_to_string(root.join("intel_pstate/no_turbo")).unwrap(),
            "0"
        );

        let state = cpu.state(Some(CpuProfile::Performance));
        assert_eq!(state.effective, Some(CpuProfile::Performance));
        assert_eq!(state.profile, Some(CpuProfile::Performance));

        cpu.apply(CpuProfile::Efficiency).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("cpu0/cpufreq/energy_performance_preference")).unwrap(),
            "power"
        );
        assert_eq!(
            fs::read_to_string(root.join("intel_pstate/no_turbo")).unwrap(),
            "1"
        );
        assert_eq!(cpu.state(None).effective, Some(CpuProfile::Efficiency));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unsupported_governor_warns() {
        let root = std::env::temp_dir().join(format!("ms01ctl-cpu-warn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("cpu0/cpufreq");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("scaling_driver"), "intel_pstate\n").unwrap();
        fs::write(dir.join("scaling_governor"), "powersave\n").unwrap();
        fs::write(dir.join("scaling_available_governors"), "powersave\n").unwrap();

        let paths = Paths { cpu: root.clone(), ..Default::default() };
        let cpu = CpuControl::discover(&paths);
        assert!(!cpu.supports_profile(CpuProfile::Performance));
        // Nothing could be written at all -> hard error, not just a warning.
        let err = cpu.apply(CpuProfile::Performance).unwrap_err().to_string();
        assert!(err.contains("unsupported"), "{err}");
        // The profile that only needs 'powersave' still applies.
        assert!(cpu.supports_profile(CpuProfile::Efficiency));
        cpu.apply(CpuProfile::Efficiency).unwrap();
        fs::remove_dir_all(&root).ok();
    }
}
