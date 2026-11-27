#!/usr/bin/env bash
# Fixed-width ASCII panels for the spacecraft HUD. No writes or privileged commands.
set -euo pipefail
export LC_ALL=C
panel() {
    local title=$1 row
    shift
    printf '+- %s ' "$title"
    printf '%*s' "$((26-${#title}))" '' | tr ' ' '-'
    printf '+\n'
    for row in "$@"; do printf '| %-27.27s |\n' "$row"; done
    printf '+-----------------------------+\n'
}
case "${1:-}" in
    nav)
        panel 'NAV / FLIGHT 01' 'MODE    : CRUISE' 'VECTOR  : 125.04 DEG' 'TRACK   : AUTO' "CLOCK   : $(date '+%H:%M:%S')"
        ;;
    core)
        cpu=-- gpu=-- temp=--
        if data=$(bash "$(dirname "$0")/telemetry.sh"); then
            while read -r key value; do
                case "$key" in
                    CPU) cpu=${value%%%} ;;
                    GPU) gpu=${value%%%} ;;
                    TEMP) temp=${value%°C} ;;
                esac
            done <<< "$data"
        fi
        printf -v cpurow 'CPU BUS : %3s%%' "$cpu"
        printf -v gpurow 'GPU BUS : %3s%%' "$gpu"
        printf -v temprow 'THERMAL : %3s C' "$temp"
        status='STATUS  : ONLINE'
        if [[ "$cpu" == -- || "$gpu" == -- || "$temp" == -- ]]; then status='STATUS  : PARTIAL'; fi
        panel 'CORE / TELEMETRY' "$cpurow" "$gpurow" "$temprow" "$status"
        ;;
    uplink)
        gain=-- filled=0 status='STATUS  : NO SIGNAL'
        if command -v wpctl >/dev/null && raw=$(wpctl get-volume @DEFAULT_AUDIO_SINK@); then
            if gain=$(awk '$1=="Volume:" && $2 ~ /^[0-9]+([.][0-9]+)?$/ { printf "%d", $2*100+0.5; ok=1 } END { if (!ok) exit 1 }' <<< "$raw"); then
                filled=$(( (gain+2)/5 ))
                ((filled <= 20)) || filled=20
                status='STATUS  : ONLINE'
                if [[ "$raw" == *"[MUTED]"* ]]; then status='STATUS  : MUTED'; filled=0; fi
            else gain=--; fi
        fi
        printf -v gainrow 'GAIN    : %3s%%' "$gain"
        printf -v bar '%*s' "$filled" ''
        bar=${bar// /#}
        printf -v empty '%*s' "$((20-filled))" ''
        empty=${empty// /-}
        panel 'COMMS / UPLINK' 'CHANNEL : 01' "$gainrow" "[$bar$empty]" "$status"
        ;;
    *) printf 'Usage: %s {nav|core|uplink}\n' "$0" >&2; exit 2 ;;
esac
