#!/usr/bin/env bash
# Fixed-width ASCII panels for the spacecraft HUD. No writes or privileged commands.
set -euo pipefail
export LC_ALL=C
panel() {
    local title=$1 row
    shift
    # Monospace fixed-width canvas. Padding is ASCII; the decorative glyphs
    # are literal UTF-8, so the C locale cannot truncate them mid-byte.
    printf '┌─ %s\n' "$title"
    for row in "$@"; do printf '  %s\n' "$row"; done
    printf '└──────────────────────────\n'
}
case "${1:-}" in
    nav)
        panel '01 / NAVIGATION' 'CRUISE         AUTO' 'HDG         125.04°' "UTC        $(date -u '+%H:%M:%S')"
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
        printf -v cpurow 'CPU            %3s%%' "$cpu"
        printf -v gpurow 'GPU            %3s%%' "$gpu"
        printf -v temprow 'THERMAL       %3s C' "$temp"
        panel '02 / SYSTEMS' "$cpurow" "$gpurow" "$temprow"
        ;;
    uplink)
        gain=-- filled=0 status='NO SIGNAL'
        if command -v wpctl >/dev/null && raw=$(wpctl get-volume @DEFAULT_AUDIO_SINK@); then
            if gain=$(awk '$1=="Volume:" && $2 ~ /^[0-9]+([.][0-9]+)?$/ { printf "%d", $2*100+0.5; ok=1 } END { if (!ok) exit 1 }' <<< "$raw"); then
                filled=$(( (gain*14+50)/100 ))
                ((filled <= 14)) || filled=14
                status='ONLINE'
                if [[ "$raw" == *"[MUTED]"* ]]; then status='MUTED'; filled=0; fi
            else gain=--; fi
        fi
        printf -v gainrow 'OUTPUT         %3s%%' "$gain"
        printf -v bar '%*s' "$filled" ''
        bar=${bar// /▰}
        printf -v empty '%*s' "$((14-filled))" ''
        empty=${empty// /░}
        panel '03 / AUDIO LINK' "$gainrow" "$bar$empty" "$status"
        ;;
    *) printf 'Usage: %s {nav|core|uplink}\n' "$0" >&2; exit 2 ;;
esac
