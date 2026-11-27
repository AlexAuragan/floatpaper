#!/usr/bin/env bash
# Linux telemetry demo. No dependencies beyond Bash and procfs/sysfs.
# CPU utilization is sampled across a short interval, not load average.
set -euo pipefail

cpu_sample() {
    local label user nice system idle iowait irq softirq steal rest
    read -r label user nice system idle iowait irq softirq steal rest < /proc/stat
    printf '%s %s\n' "$((user + nice + system + idle + iowait + irq + softirq + steal))" "$((idle + iowait))"
}

read -r total0 idle0 < <(cpu_sample)
sleep 0.15
read -r total1 idle1 < <(cpu_sample)
delta=$((total1 - total0))
cpu='--'
if ((delta > 0)); then
    cpu=$((100 * (delta - idle1 + idle0) / delta))
fi

# Use the busiest AMD GPU exposing gpu_busy_percent, or '--' if unavailable.
gpu='--'
for file in /sys/class/drm/card*/device/gpu_busy_percent; do
    [[ -r "$file" ]] || continue
    if read -r value < "$file" && [[ "$value" =~ ^[0-9]+$ ]]; then
        if [[ "$gpu" == '--' ]] || ((value > gpu)); then gpu=$value; fi
    fi
done

# CPU package temperature where exposed by Intel coretemp or AMD k10temp.
temp='--'
for dir in /sys/class/hwmon/hwmon*; do
    [[ -r "$dir/name" ]] || continue
    read -r driver < "$dir/name" || continue
    [[ "$driver" == coretemp || "$driver" == k10temp ]] || continue
    for label_file in "$dir"/temp*_label; do
        [[ -r "$label_file" ]] || continue
        read -r label < "$label_file" || continue
        [[ "$label" == 'Package id '* || "$label" == Tctl || "$label" == Tdie ]] || continue
        input="${label_file%_label}_input"
        if [[ -r "$input" ]] && read -r value < "$input" && [[ "$value" =~ ^[0-9]+$ ]]; then
            degrees=$((value / 1000))
            if [[ "$temp" == '--' ]] || ((degrees > temp)); then temp=$degrees; fi
        fi
    done
done
printf 'CPU  %3s%%\nGPU  %3s%%\nTEMP %3s°C\n' "$cpu" "$gpu" "$temp"
