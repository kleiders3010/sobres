#!/usr/bin/env bash
# Battery cost of the two pending choices (DESIGN §8, D47 "More often" checks; D23 per-group
# network identity), measured with Android's own batterystats on a real phone.
#
# Usage: scripts/battery-measure.sh SERIAL SCENARIO DURATION [options]
#   SCENARIO  baseline | more-often | endpoints-N | more-often+endpoints-N
#             (endpoints-N: N network endpoints, the real one plus N-1 idle extras, as with
#             one per group for N groups)
#   DURATION  e.g. 8h, 90m, 600s (how long the phone stays unplugged and idle)
#   --interval MIN   minutes between "more often" checks (default 20). In Doze, Android 12+
#                    fires an app's inexact allow-while-idle alarms at most 7 times an hour
#                    (allow_while_idle_compat_quota, seen on the emulator), so keep it >= 9.
#   --keep           leave the scenario on afterwards (default: back to off)
#   --force-idle     emulator only: force Doze right after the reset (adb stays connected)
#   --collect RUNDIR only collect (after an interrupted run); RUNDIR under target/battery
#
# The scenario is switched through the app's hidden measurement receiver (Measure.kt),
# which only the adb shell can reach (android.permission.DUMP), so a release-signed app
# works. Results: one CSV row per run in target/battery/results.csv, raw dumps in
# target/battery/RUN/.
#
# What biases the numbers, and what this script does about it:
# - Batterystats doesn't count while charging. The phone must be unplugged for the whole
#   run; the reset happens only after it reports no power source, and all rates are per
#   hour *on battery* (from batterystats' own clock), not per wall-clock hour.
# - Wireless debugging keeps Wi-Fi and the CPU busier than a normal idle phone (adb
#   keep-alives, mDNS). An awake Wi-Fi radio makes the app's own network wakes look
#   cheaper (the radio is already up) and keeps the device out of deep Doze, where the
#   "more often" alarm's cost shows. So adb is used only at the start and the end: the
#   script disconnects right after the reset, and the owner turns Wireless debugging off
#   for the run and back on at the end.
# - The end (unlocking, toggling Wireless debugging) adds a little screen-on time. The app's
#   per-UID numbers barely change (it isn't opened); device totals do, so the CSV keeps the
#   screen-off and deep-Doze discharge separately.
set -euo pipefail
cd "$(dirname "$0")/.."

ADB=~/AppData/Local/Android/Sdk/platform-tools/adb.exe
PKG=${KT_PKG:-com.jurisgames.sobres}
# --pair PKG:SCENARIO measures a second copy of the app in the same run (same reset, same
# conditions), e.g. the measure build under .m20 against the one under .m1.
PAIR_PKG=""
PAIR_SCENARIO=""
S=${1:?serial}
SCENARIO=${2:?scenario}
DURATION=${3:?duration}
shift 3
INTERVAL=20
KEEP=0
FORCE_IDLE=0
COLLECT_ONLY=""
while [ $# -gt 0 ]; do
  case "$1" in
    --interval) INTERVAL=${2:?minutes}; shift 2 ;;
    --keep) KEEP=1; shift ;;
    --force-idle) FORCE_IDLE=1; shift ;;
    --collect) COLLECT_ONLY=${2:?run dir}; shift 2 ;;
    --pair) PAIR_PKG=${2%%:*}; PAIR_SCENARIO=${2#*:}; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done

# Scenario -> "minutes-between-checks endpoints"
parse_scenario() {
  local m=0 e=1 part
  for part in ${1//+/ }; do
    case "$part" in
      baseline) ;;
      more-often) m=$INTERVAL ;;
      endpoints-[0-9]*) e=${part#endpoints-} ;;
      *) echo "unknown scenario part: $part" >&2; exit 2 ;;
    esac
  done
  echo "$m $e"
}
read -r MINUTES ENDPOINTS <<<"$(parse_scenario "$SCENARIO")"
[ -n "$PAIR_PKG" ] && read -r PAIR_MINUTES PAIR_ENDPOINTS <<<"$(parse_scenario "$PAIR_SCENARIO")"

case "$DURATION" in
  *h) SECS=$(( ${DURATION%h} * 3600 )) ;;
  *m) SECS=$(( ${DURATION%m} * 60 )) ;;
  *s) SECS=${DURATION%s} ;;
  *) SECS=$(( DURATION * 60 )) ;;
esac

EMU=0
[[ $S == emulator-* ]] && EMU=1
SERIALNO=""
HOST=""
[[ $S == *:* ]] && HOST=${S%%:*}

say() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*"; }

# One adb call, bounded: wireless debugging drops constantly, so every call gets its own
# short timeout, and a dropped phone is found again (below) and the call retried once.
raw() { MSYS_NO_PATHCONV=1 timeout 15 "$ADB" "$@"; }
online() { [ "$(raw -s "$S" get-state 2>/dev/null | tr -d '\r')" = "device" ]; }

# Find the phone again: its Wireless debugging port changes whenever it is toggled. mDNS
# (_adb-tls-connect) names it adb-<serialno>-<random>; match that, or its IP.
reconnect() {
  online && return 0
  [ $EMU = 1 ] && return 1
  local line addr pat=""
  [ -n "$SERIALNO" ] && pat="^adb-$SERIALNO-"
  [ -n "$HOST" ] && pat="${pat:+$pat|}[[:space:]]$HOST:"
  [ -n "$pat" ] || return 1
  line=$(raw mdns services 2>/dev/null | tr -d '\r' | grep "_adb-tls-connect" | grep -E "$pat" | head -1 || true)
  addr=$(printf '%s' "$line" | awk '{print $NF}')
  [ -n "$addr" ] || return 1
  raw connect "$addr" >/dev/null 2>&1 || return 1
  S=$addr
  HOST=${addr%%:*}
  online
}

a() {
  local out
  if out=$(raw -s "$S" "$@" 2>&1); then printf '%s\n' "$out"; return 0; fi
  online && { printf '%s\n' "$out"; return 1; } # the command failed, not the connection
  reconnect || return 1
  raw -s "$S" "$@"
}

# Keep trying to find the phone (the owner is turning Wireless debugging back on).
wait_for_phone() {
  local until=$(( $(date +%s) + ${1:-3600} ))
  until reconnect; do
    [ "$(date +%s)" -lt "$until" ] || { say "phone not found; rerun with --collect $RUN"; exit 1; }
    sleep 15
  done
}

measure_pkg() { # measure_pkg PKG MINUTES ENDPOINTS -> that app's status line
  local out pkg=$1
  shift
  # The receiver's class and action keep their names in every copy of the app.
  out=$(a shell am broadcast --include-stopped-packages -n "$pkg/com.jurisgames.sobres.MeasureReceiver" -a com.jurisgames.sobres.MEASURE ${1:+--ei more_often_min "$1" --ei endpoints "$2"} | tr -d '\r')
  printf '%s\n' "$out" | sed -n 's/.*data="\(.*\)".*/\1/p'
}
measure() { measure_pkg "$PKG" "$@"; }
uid_of() { a shell pm list packages -U "$1" | tr -d '\r' | awk -v p="package:$1" '$1==p {sub("uid:", "", $2); print $2}'; }

on_power() { # prints the power sources that are on, if any
  a shell dumpsys battery | tr -d '\r' | awk -F': ' '/ powered: true/ {sub(/^ +/, "", $1); print $1}'
}

collect() {
  say "collecting"
  local dir=$1
  a shell dumpsys batterystats --checkin | tr -d '\r' >"$dir/checkin.txt"
  a shell dumpsys batterystats "$PKG" | tr -d '\r' >"$dir/batterystats.txt" || true
  a shell dumpsys battery | tr -d '\r' >"$dir/battery-end.txt"
  a shell dumpsys alarm | tr -d '\r' | grep -A3 "$PKG" >"$dir/alarm.txt" || true
  a shell dumpsys deviceidle | tr -d '\r' | grep -E "mState|mLightState|mDeepEnabled|mForceIdle" >"$dir/deviceidle.txt" || true
  a shell am get-standby-bucket "$PKG" | tr -d '\r' >"$dir/standby-bucket.txt" || true
  measure "" "" >"$dir/status-end.txt"
  date -u +%Y-%m-%dT%H:%M:%SZ >"$dir/end-utc.txt"
  report "$dir"
  if [ -f "$dir/pair/pkg.txt" ]; then
    local p
    p=$(cat "$dir/pair/pkg.txt")
    cp "$dir/checkin.txt" "$dir/battery-end.txt" "$dir/end-utc.txt" "$dir/pair/"
    a shell am get-standby-bucket "$p" | tr -d '\r' >"$dir/pair/standby-bucket.txt" || true
    measure_pkg "$p" "" "" >"$dir/pair/status-end.txt"
    report "$dir/pair"
  fi
}

# One CSV row from the checkin dump (fields: AOSP BatteryStats.dumpCheckinLocked).
report() {
  local dir=$1 uid
  uid=$(cat "$dir/uid.txt")
  local csv=target/battery/results.csv
  [ -f "$csv" ] || echo "run,scenario,serial,model,sdk,start_utc,end_utc,on_battery_min,screen_off_min,screen_on_min,deep_idle_min,battery_start_pct,battery_end_pct,device_discharge_mah,screen_off_mah,deep_doze_mah,battery_capacity_mah,app_mah,app_mah_per_day,app_pct_capacity_per_day,app_cpu_ms,app_wakelock_ms,app_wakelocks,app_wakeup_alarms,app_jobs,app_job_ms,app_mobile_active_ms,app_mobile_wakeups,app_wifi_active_ms,app_wifi_wakeups,app_mobile_bytes,app_wifi_bytes,alarms_fired,rounds,round_ms_total,extra_online,net_changes,standby_bucket" >"$csv"
  local status
  status=$(cat "$dir/status-end.txt")
  kv() { printf '%s\n' "$status" | tr ' ' '\n' | sed -n "s/^$1=//p"; }
  local level_end
  level_end=$(awk -F': ' '/^ *level:/ {print $2; exit}' "$dir/battery-end.txt")
  awk -F, -v uid="$uid" '
    $4=="bt" && $2==0 { onbat=$6; scroff=$11 }
    $4=="m" && $2==0 { scron=$5; deep=$14 }
    $4=="dc" && $2==0 { dis=$9; off=$10; deepmah=$14 }
    $4=="pws" && $2==0 { cap=$5 }
    $2!=uid { next }
    $4=="pwi" && $5=="uid" { mah+=$6 }
    $4=="cpu" { cpu+=$5+$6 }
    $4=="wl" { for (i=6;i<=NF;i++) if ($i=="p") { wlms+=$(i-1); wln+=$(i+1) } }
    $4=="wua" { wua+=$6 }
    $4=="jb" { jobs+=$7; jobms+=$6 }
    $4=="nt" { mobb+=$5+$6; wifib+=$7+$8; mobact+=$13; mobwake+=$17; wifiwake+=$18 }
    $4=="wfcd" { wifiact+=$6; for (i=8;i<=NF;i++) wifiact+=$i }
    END {
      day = (onbat>0) ? 86400000/onbat : 0
      printf "%.1f,%.1f,%.1f,%.1f,%s,%s,%s,%s,%.4f,%.3f,%.4f,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d\n",
        onbat/60000, scroff/60000, scron/60000, deep/60000, dis+0, off+0, deepmah+0, cap+0,
        mah, mah*day, (cap>0 ? 100*mah*day/cap : 0), cpu, wlms, wln, wua, jobs, jobms,
        mobact, mobwake, wifiact, wifiwake, mobb, wifib
    }' "$dir/checkin.txt" >"$dir/app.csv"
  local a1 rest
  a1=$(cut -d, -f1-4 "$dir/app.csv")      # on_battery .. deep_idle
  rest=$(cut -d, -f5- "$dir/app.csv")     # discharge .. bytes
  echo "$(basename "$dir"),$(cat "$dir/scenario.txt"),$(cat "$dir/serial.txt"),$(cat "$dir/model.txt"),$(cat "$dir/sdk.txt"),$(cat "$dir/start-utc.txt"),$(cat "$dir/end-utc.txt"),$a1,$(cat "$dir/battery-start.txt"),$level_end,$rest,$(kv alarms),$(kv rounds),$(kv round_ms_total),$(kv extra_online),$(kv net_changes),$(awk '{print $1}' "$dir/standby-bucket.txt")" >>"$csv"
  say "result: $csv"
  tail -1 "$csv" | tr ',' '\n' | paste -d= <(head -1 "$csv" | tr ',' '\n') - | sed 's/^/  /'
}

if [ -n "$COLLECT_ONLY" ]; then
  RUN=$COLLECT_ONLY
  S=$(cat "$RUN/serial.txt")
  SERIALNO=$(cat "$RUN/serialno.txt")
  [[ $S == *:* ]] && HOST=${S%%:*}
  wait_for_phone 600
  collect "$RUN"
  [ $KEEP = 1 ] || say "scenario off: $(measure 0 1)"
  [ $KEEP = 1 ] || [ ! -f "$RUN/pair/pkg.txt" ] || say "pair off: $(measure_pkg "$(cat "$RUN/pair/pkg.txt")" 0 1)"
  exit 0
fi

# --- Start ------------------------------------------------------------------------------
online || reconnect || { echo "device $S not connected" >&2; exit 1; }
SERIALNO=$(a shell getprop ro.serialno | tr -d '\r')
UID_=$(uid_of "$PKG")
[ -n "$UID_" ] || { echo "$PKG not installed on $S" >&2; exit 1; }
if [ -n "$PAIR_PKG" ]; then
  PAIR_UID=$(uid_of "$PAIR_PKG")
  [ -n "$PAIR_UID" ] || { echo "$PAIR_PKG not installed on $S" >&2; exit 1; }
fi
RUN=target/battery/$(date +%Y%m%d-%H%M)-$SCENARIO
mkdir -p "$RUN"
echo "$SCENARIO" >"$RUN/scenario.txt"
echo "$S" >"$RUN/serial.txt"
echo "$SERIALNO" >"$RUN/serialno.txt"
echo "$UID_" >"$RUN/uid.txt"
a shell getprop ro.product.model | tr -d '\r' | tr ',' ' ' >"$RUN/model.txt"
a shell getprop ro.build.version.sdk | tr -d '\r' >"$RUN/sdk.txt"
say "$SCENARIO on $(cat "$RUN/model.txt") (uid $UID_), $DURATION unplugged; raw data in $RUN"

if [ -n "$PAIR_PKG" ]; then
  mkdir -p "$RUN/pair"
  echo "$PAIR_PKG" >"$RUN/pair/pkg.txt"
  echo "$PAIR_SCENARIO ($PAIR_PKG)" >"$RUN/pair/scenario.txt"
  echo "$PAIR_UID" >"$RUN/pair/uid.txt"
  for f in serial.txt serialno.txt model.txt sdk.txt; do cp "$RUN/$f" "$RUN/pair/"; done
  pstatus=$(measure_pkg "$PAIR_PKG" "$PAIR_MINUTES" "$PAIR_ENDPOINTS")
  say "pair set: $pstatus"
  case "$pstatus" in *"more_often_min=$PAIR_MINUTES endpoints=$PAIR_ENDPOINTS"*) ;; *) echo "the pair app didn't take its scenario" >&2; exit 1 ;; esac
fi
status=$(measure "$MINUTES" "$ENDPOINTS")
say "scenario set: $status"
case "$status" in *"more_often_min=$MINUTES endpoints=$ENDPOINTS"*) ;; *) echo "the app didn't take the scenario" >&2; exit 1 ;; esac
echo "$status" >"$RUN/status-start.txt"
if [ "$ENDPOINTS" -gt 1 ]; then
  # The extra endpoints need a moment to reach their home relays.
  for _ in 1 2 3 4; do
    case "$status" in *"extra_online=$((ENDPOINTS - 1)) "*) break ;; esac
    sleep 10
    status=$(measure "" "")
  done
  say "extra endpoints: $(printf '%s' "$status" | tr ' ' '\n' | grep -E '^extra_' | tr '\n' ' ')"
fi

if [ $EMU = 1 ]; then
  a shell dumpsys battery unplug >/dev/null # the emulator is always "charging"
else
  say "UNPLUG the phone now (no charger, no USB). Waiting until it reports no power source..."
  until [ -z "$(on_power)" ]; do sleep 10; done
fi
a shell dumpsys batterystats --reset >/dev/null
date -u +%Y-%m-%dT%H:%M:%SZ >"$RUN/start-utc.txt"
a shell dumpsys battery | tr -d '\r' | awk -F': ' '/^ *level:/ {print $2; exit}' >"$RUN/battery-start.txt"
say "batterystats reset at $(cat "$RUN/battery-start.txt")% battery"
[ -d "$RUN/pair" ] && cp "$RUN/start-utc.txt" "$RUN/battery-start.txt" "$RUN/pair/"

if [ $EMU = 1 ]; then
  [ $FORCE_IDLE = 1 ] && a shell cmd deviceidle force-idle >/dev/null && say "emulator forced into Doze"
else
  raw disconnect "$S" >/dev/null 2>&1 || true
  cat <<EOF

  Now, on the phone:
    1. Settings > Developer options > turn OFF Wireless debugging (adb keeps Wi-Fi busy).
    2. Lock the screen and leave the phone still, unplugged, on its usual Wi-Fi.
       Don't open Tasks; ignore it for $DURATION (until $(date -d "+$SECS seconds" +%H:%M 2>/dev/null || echo "$DURATION from now")).
    3. Then turn Wireless debugging back ON (still unplugged). This script finds it again.
  (If this window closes, collect later: $0 $S $SCENARIO $DURATION --collect $RUN)

EOF
fi

end=$(( $(date +%s) + SECS ))
while [ "$(date +%s)" -lt "$end" ]; do
  left=$(( end - $(date +%s) ))
  say "waiting: $(( left / 60 )) min left"
  sleep $(( left < 600 ? left : 600 ))
done

# --- End --------------------------------------------------------------------------------
[ $EMU = 1 ] || say "time's up: turn Wireless debugging back ON (keep the phone unplugged)"
wait_for_phone 3600
[ $EMU = 1 ] || [ -z "$(on_power)" ] || say "warning: the phone is on power now; numbers stop at the plug-in"
collect "$RUN"
if [ $EMU = 1 ]; then
  a shell cmd deviceidle unforce >/dev/null || true
  a shell dumpsys battery reset >/dev/null
fi
[ $KEEP = 1 ] || say "scenario off: $(measure 0 1)"
[ $KEEP = 1 ] || [ -z "$PAIR_PKG" ] || say "pair off: $(measure_pkg "$PAIR_PKG" 0 1)"
