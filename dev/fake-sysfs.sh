#!/usr/bin/env bash
# Build a fake sysfs tree that mirrors an MS-01, so the whole service —
# fan control loop, NVMe wear sampling and CPU profile switching — can be
# exercised on any Linux machine without the real hardware.
#
#   ./dev/fake-sysfs.sh [root]     # default root: /tmp/ms01ctl-fake
#   source /tmp/ms01ctl-fake/env.sh
#   cd server && cargo run
#
# Everything (config, SQLite history, chip snapshot) is written under the
# fake root; the real /sys and /etc are never touched.
set -euo pipefail

root="${1:-/tmp/ms01ctl-fake}"
sys="$root/sys"

rm -rf "$root"
mkdir -p "$sys"/{hwmon,nvme,cpu,bin} "$root"/{etc,var}

# --- fan controller (nct6798): two fans, five Smart Fan IV points each ------
fan="$sys/hwmon/hwmon5"
mkdir -p "$fan"
echo nct6798 > "$fan/name"
for i in 1 2; do
    echo 5 > "$fan/pwm${i}_enable"
    echo 128 > "$fan/pwm$i"
    echo 1150 > "$fan/fan${i}_input"
    for p in 1 2 3 4 5; do
        echo $((40000 + p * 10000)) > "$fan/pwm${i}_auto_point${p}_temp"
        echo $((p * 45)) > "$fan/pwm${i}_auto_point${p}_pwm"
    done
done

# --- CPU (coretemp): package + 19 cores ------------------------------------
cpu="$sys/hwmon/hwmon4"
mkdir -p "$cpu"
echo coretemp > "$cpu/name"
echo 52000 > "$cpu/temp1_input"
echo "Package id 0" > "$cpu/temp1_label"
for i in $(seq 2 20); do
    echo $((44000 + i * 300)) > "$cpu/temp${i}_input"
    echo "Core $((i - 2))" > "$cpu/temp${i}_label"
done

# --- NVMe controllers and their hwmon composite sensors --------------------
declare -A models=(
    [nvme0]="Force MP510"
    [nvme1]="Samsung SSD 990 PRO 4TB"
    [nvme2]="Samsung SSD 990 PRO 4TB"
)
declare -A serials=(
    [nvme0]="204782480001291711F6"
    [nvme1]="S7DPNF0Y612074T"
    [nvme2]="S7DPNF0Y116807Y"
)
hwmon=1
for dev in nvme0 nvme1 nvme2; do
    mkdir -p "$sys/nvme/$dev"
    echo "${models[$dev]}" > "$sys/nvme/$dev/model"
    echo "${serials[$dev]}" > "$sys/nvme/$dev/serial"
    echo "4B2QJXD7" > "$sys/nvme/$dev/firmware_rev"

    # hwmon1..3: the per-controller composite temperature sensor.
    h="$sys/hwmon/hwmon$hwmon"
    mkdir -p "$h"
    echo nvme > "$h/name"
    echo $((36000 + hwmon * 3000)) > "$h/temp1_input"
    ln -s "$sys/nvme/$dev" "$h/device"
    hwmon=$((hwmon + 1))
done

# --- cpufreq (intel_pstate) with EPP and a turbo knob ----------------------
for c in 0 1 2 3; do
    p="$sys/cpu/cpu$c/cpufreq"
    mkdir -p "$p"
    echo intel_pstate > "$p/scaling_driver"
    echo powersave > "$p/scaling_governor"
    echo "performance powersave" > "$p/scaling_available_governors"
    echo balance_power > "$p/energy_performance_preference"
    echo "default performance balance_performance balance_power power" \
        > "$p/energy_performance_available_preferences"
done
mkdir -p "$sys/cpu/intel_pstate"
echo 1 > "$sys/cpu/intel_pstate/no_turbo"

# --- fake smartctl with plausible NVMe SMART logs --------------------------
cat > "$sys/bin/smartctl" <<'SH'
#!/bin/bash
# Minimal stand-in for `smartctl -A -j /dev/nvmeN`.
dev="${@: -1}"
case "${dev#/dev/nvme}" in
0) pct=3  hours=18875 written=14073992 cycles=6822 unsafe=188 temp=48 thresh=5 ;;
1) pct=4  hours=4530  written=73476239 cycles=140  unsafe=30  temp=37 thresh=10 ;;
2) pct=4  hours=4542  written=75937986 cycles=173  unsafe=65  temp=42 thresh=10 ;;
*) echo '{"smartctl":{"exit_status":2,"messages":[{"string":"device not found","severity":"error"}]}}'
   exit 1 ;;
esac
cat <<JSON
{"json_format_version": [1, 0],
 "smartctl": {"exit_status": 0},
 "device": {"name": "$dev", "type": "nvme", "protocol": "NVMe"},
 "nvme_smart_health_information_log": {
   "nsid": -1, "critical_warning": 0, "temperature": $temp,
   "available_spare": 100, "available_spare_threshold": $thresh,
   "percentage_used": $pct, "data_units_written": $written,
   "power_cycles": $cycles, "power_on_hours": $hours,
   "unsafe_shutdowns": $unsafe, "media_errors": 0, "num_err_log_entries": 1}}
JSON
SH
chmod +x "$sys/bin/smartctl"

cat > "$root/env.sh" <<EOF
# Source this to run ms01ctl against the fake sysfs tree.
export MS01CTL_CONFIG="$root/etc/config.json"
export MS01CTL_DB="$root/var/history.db"
export MS01CTL_LISTEN="127.0.0.1:8090"
export MS01CTL_SYSFS_HWMON="$sys/hwmon"
export MS01CTL_SYSFS_NVME="$sys/nvme"
export MS01CTL_SYSFS_CPU="$sys/cpu"
export MS01CTL_SMARTCTL="$sys/bin/smartctl"
EOF

echo "fake MS-01 sysfs ready in $root"
echo "next: source $root/env.sh && cd server && cargo run"
