# ms01ctl — MS-01 monitoring & control

Background service + web UI for a MinisForum MS-01 running Linux. One small
Rust (axum) binary does three jobs and serves a Svelte frontend:

1. **Fan control** — drives the two chassis fans through the nct6798 Super I/O
   chip following a draggable fan curve.
2. **NVMe wear monitoring** — reads SMART endurance data of every NVMe drive
   and keeps the history in a local SQLite database.
3. **CPU power profiles** — switches the CPU between performance- and
   efficiency-oriented settings (governor, energy-performance preference and
   turbo).

All hardware access is through sysfs (plus `smartctl` for the wear data) —
no kernel modules, no long-running subprocesses.

## Fan control

- **Hardware discovery by chip name**, not hwmon index (`hwmon7` can change
  between boots): the fan controller is found as `nct67*`, the CPU sensors as
  `coretemp` (package + all cores) and the NVMe drives as `nvme` (Composite).
  Temperatures are read directly from sysfs — no `sensors` subprocess. NVMe
  model names come from `/sys/class/nvme/*/model`.
- **Control temperature** = CPU package temperature only. Core and NVMe
  temperatures are displayed but do not influence the fans — NVMe composite
  readings have 1 °C resolution (integer Kelvin per the NVMe spec) and react
  too slowly to be useful control inputs.
- **Per-fan modes**:
  - `auto` — firmware control. The `pwmN_enable` value and Smart Fan IV
    curve points observed at service startup are restored. Writing `0`
    would not work here: on the nct6775 driver `0` means "full speed, no
    control" — restoring the original values is what actually returns the
    fan to BIOS automatic control.
  - `curve` — `pwmN_enable=1` (manual) and the daemon drives the duty along
    the configured curve with linear interpolation between points, clamped
    at the ends.
  - `hardware` — the user's curve is programmed into the chip's Smart Fan IV
    engine (`pwmN_auto_point*`) and the hardware runs the control loop
    autonomously: it keeps working even if the daemon or the whole OS dies.
    Chip constraints, reflected in the editor: exactly 5 points,
    non-decreasing duty, and the input is the board's CPU sensor (CPUTIN,
    typically a few °C below the package temperature).
  - `manual` — fixed duty cycle.
- **Firmware state snapshot**: the BIOS-programmed fan settings are captured
  at the first start of each boot and persisted
  (`/etc/ms01ctl/chip-snapshot.json`, keyed by boot id), so `auto` restores
  the true BIOS curve even after the daemon crashed while in hardware mode.
- **Failsafes**: if no temperature can be read in curve mode the fans go to
  100%; on shutdown (SIGTERM/Ctrl-C) all fans are returned to hardware auto.
- **Config** is persisted as JSON at `/etc/ms01ctl/config.json` (override
  with `MS01CTL_CONFIG`), written atomically, validated on load and on every
  API update.

If the nct67xx chip is missing (driver not loaded, unsupported board) the
service still starts: fan control is disabled and the monitoring features
keep working.

## NVMe wear monitoring

- Wear data is read with `smartctl -A -j /dev/nvmeN` (JSON), falling back to
  `nvme smart-log --output-format=json` when smartctl is unavailable. Captured
  per drive: percentage used, bytes/units written, power-on hours, power
  cycles, unsafe shutdowns, available spare, media errors, error-log entries,
  composite temperature and the SMART critical-warning flag.
- **History**: readings are appended to SQLite at
  `/var/lib/ms01ctl/history.db` (override or disable with `MS01CTL_DB`;
  WAL mode, one row per drive). A row is only written when the endurance
  figures (percentage used, bytes written, spare, media errors, critical
  warning) actually changed, so manual refreshes do not flood the table.
  The sampling interval (`wear_sample_interval_secs`, default 3600) and the
  retention (`wear_retention_days`, default 1095; `0` keeps everything) are
  part of the config. The daemon always samples once at start-up and on
  demand.
- The UI shows the current values, a wear sparkline over a selectable window
  (7 days … 3 years) and a wear-rate projection derived from the stored
  history ("x %/month · ≈ y months to 100%").
- If SMART cannot be read (missing `smartmontools`, permissions, unsupported
  device) the drive is still listed and the error is shown in the UI — the
  rest of the service is unaffected.

## CPU power profiles

Three profiles, applied to every cpufreq policy and the turbo knob:

| Profile      | `scaling_governor` | EPP hint              | Turbo |
| ------------ | ------------------ | --------------------- | ----- |
| `performance`| `performance`      | `performance`         | on    |
| `balanced`   | `powersave`        | `balance_performance` | on    |
| `efficiency` | `powersave`        | `power`               | off   |

- EPP (`energy_performance_preference`) is the knob that matters on
  `intel_pstate`/`amd_pstate` in active mode; the turbo knob is
  `intel_pstate/no_turbo` or `cpufreq/boost`.
- Knobs that do not exist or reject a value produce a warning in the UI, not a
  failure — profiles are only offered when the CPU advertises the required
  governor/EPP values.
- The selected profile is stored in the config (`cpu.profile`) and re-applied
  at every start-up. `null` means "not managed": the daemon only displays the
  live settings and never writes them. Selecting a profile takes effect
  immediately and is persisted right away (it is not part of the fan
  "Save & Apply" flow).
- Note: the `performance`/`balanced` profiles turn turbo on and `efficiency`
  turns it off — that is the point of the switch, but it does override a
  kernel command line or a previous manual setting.

## Web UI

`http://<host>:8090` (override with `MS01CTL_LISTEN`, e.g. `127.0.0.1:9000`).

- Live temperatures polled every 2 s: CPU package (the control input) with
  all per-core temps, plus fan RPM / duty per fan.
- CPU power profile switch with the live driver/governor/EPP/turbo values.
- One card per NVMe drive: wear bar, endurance left, bytes written, power-on
  time, spare, media errors, unsafe shutdowns, power cycles, temperature,
  wear history sparkline and a Refresh button (reads SMART immediately).
- Per-fan mode switch (Auto / Curve / Manual / HW curve) and manual speed
  slider.
- Interactive curve editor: drag points, double-click empty space to add a
  point, double-click a point to remove it. The orange marker shows the
  current operating point; the dashed line marks the ≈16 % duty below which
  the fans don't spin (raw PWM ≈ 40).
- Fan changes apply only on **Save & Apply** and are then persisted; the CPU
  profile and the drive refresh act immediately.

## API

| Method | Path                   | Description                                        |
| ------ | ---------------------- | -------------------------------------------------- |
| GET    | `/api/status`          | Temperatures, RPM, duty, mode, drives, CPU state   |
| GET    | `/api/config`          | Current configuration                              |
| PUT    | `/api/config`          | Validate, persist and apply a new fan config       |
| PUT    | `/api/cpu`             | Apply + persist `{"profile": "performance"\|…\|null}` |
| POST   | `/api/drives/refresh`  | Read SMART now, store a sample, return the drives  |
| GET    | `/api/history`         | Wear history; `?days=90&device=nvme0&max_points=400` |

## Configuration

`/etc/ms01ctl/config.json` (created with defaults on first start):

```json
{
  "poll_interval_ms": 2000,
  "cpu": { "profile": null },
  "wear_sample_interval_secs": 3600,
  "wear_retention_days": 1095,
  "fans": [
    {
      "mode": "auto",
      "manual_pwm": 50.0,
      "curve": [
        { "temp": 40.0, "pwm": 20.0 },
        { "temp": 50.0, "pwm": 30.0 },
        { "temp": 60.0, "pwm": 45.0 },
        { "temp": 70.0, "pwm": 65.0 },
        { "temp": 80.0, "pwm": 85.0 },
        { "temp": 85.0, "pwm": 100.0 }
      ],
      "hw_curve": [
        { "temp": 40.0, "pwm": 20.0 },
        { "temp": 55.0, "pwm": 30.0 },
        { "temp": 70.0, "pwm": 50.0 },
        { "temp": 80.0, "pwm": 75.0 },
        { "temp": 90.0, "pwm": 100.0 }
      ]
    }
  ]
}
```

Configs written by older versions load unchanged: `cpu`, `wear_*` and
`hw_curve` fall back to their defaults.

## Environment variables

| Variable              | Default                       | Purpose                                    |
| --------------------- | ----------------------------- | ------------------------------------------ |
| `MS01CTL_CONFIG`      | `/etc/ms01ctl/config.json`    | Config file (and its `chip-snapshot.json`) |
| `MS01CTL_LISTEN`      | `0.0.0.0:8090`                | HTTP listen address                        |
| `MS01CTL_DB`          | `/var/lib/ms01ctl/history.db` | Wear history DB; empty string disables it  |
| `MS01CTL_SYSFS_HWMON` | `/sys/class/hwmon`            | hwmon root (testing)                       |
| `MS01CTL_SYSFS_NVME`  | `/sys/class/nvme`             | NVMe controller root (testing)             |
| `MS01CTL_SYSFS_CPU`   | `/sys/devices/system/cpu`     | cpufreq root (testing)                     |
| `MS01CTL_SMARTCTL`    | `smartctl`                    | smartctl binary                            |
| `MS01CTL_NVME_CLI`    | `nvme`                        | nvme-cli binary (fallback)                 |
| `RUST_LOG`            | `ms01ctl=info`                | Log filter                                 |

## Building

Toolchains (Rust + Node) are self-contained in `.toolchain/` — nothing is
installed system-wide (see `UNINSTALL.md`).

```bash
./build.sh     # builds frontend, embeds it into the release binary
```

The release binary is fully self-contained (static assets embedded via
rust-embed, SQLite compiled in): `server/target/release/ms01ctl`.

## Packaging & installing

```bash
./package.sh   # builds frontend + server and produces a .deb
./install.sh   # package.sh + apt install
```

The package (`server/target/debian/ms01ctl_*.deb`, built with cargo-deb)
contains the self-sufficient binary, the systemd unit and this README;
`postinst` enables and starts the service, `apt purge` also removes
`/etc/ms01ctl` and `/var/lib/ms01ctl` (config, CPU profile and wear
history). `smartmontools` is a dependency (wear data); `nvme-cli` is an
optional fallback.

The service starts with whatever modes are saved in the config — fresh
installs default to `auto` for both fans and `null` for the CPU profile, so
nothing changes until you pick something in the UI.

## Development

```bash
# terminal 1: server (also serves the API)
cd server && cargo run
# terminal 2: frontend with hot reload, proxies /api to :8090
cd frontend && npm run dev
```

Frontend type check: `cd frontend && npm run check`.

Without an MS-01 at hand, `dev/fake-sysfs.sh` builds a fake hwmon/NVMe/cpufreq
tree (and a fake `smartctl`) that mirrors the real machine, so the whole
service can be exercised on any Linux box:

```bash
./dev/fake-sysfs.sh          # creates /tmp/ms01ctl-fake
source /tmp/ms01ctl-fake/env.sh
cd server && cargo run
```

Run server tests with `cd server && cargo test`.

## AI usage disclaimer

This project was entirely vibecoded using AI assistants.

## License

Released into the public domain under [The Unlicense](LICENSE).
