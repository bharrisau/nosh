#!/usr/bin/env bash
# 999.1 ADAPTIVE overnight deep-fuzz driver.
# Maintains a fixed worker pool (idle priority) and HOURLY reallocates cores away
# from saturated targets onto the ones still finding new coverage. Workers are
# controlled by process-NAME count (orphan-proof) and isolated with setsid.
# Self-monitors into status.txt; exits (and the sentinel re-invokes the assistant)
# on a new crash artifact, an unexpected stall, or the deadline.
set -u

ROOT=/home/bharris/github.com/bharrisau/nosh
cd "$ROOT" || exit 99
OUT="$ROOT/fuzz/overnight"
mkdir -p "$OUT"
RESULT="$OUT/RESULT"; STATUS="$OUT/status.txt"
: > "$RESULT"
echo $$ > "$OUT/driver.pid"

POOL=9                 # total concurrent workers (half of the box's 18-worker max)
RSS=2048               # libFuzzer RSS cap — an OOM here is a REAL finding
RUNTIME=54000          # ~15h remaining (≈18h total from the 08:07Z half-capacity start)
FT_THRESH=5            # < this many NEW corpus features over an hour ⇒ "no progress"
SAT_STREAK_NEEDED=2    # consecutive flat hours before a target is retired

ALL=(quic_packet osc_accumulation codec_decode read_message decode_datagram decode_epoch_ack)
PRIORITY=(quic_packet osc_accumulation)   # large-space, slow targets — the rebalance sinks
# kernel comm names are truncated to 15 chars
declare -A COMM=( [quic_packet]=quic_packet [osc_accumulation]=osc_accumulatio [codec_decode]=codec_decode [read_message]=read_message [decode_datagram]=decode_datagram [decode_epoch_ack]=decode_epoch_ac )
# decoders are already proven saturated over the prior ~3h (flat coverage, 1e8–2e9 execs, no crash)
declare -A RETIRED=( [codec_decode]=1 [read_message]=1 [decode_datagram]=1 [decode_epoch_ack]=1 )
declare -A FT_LAST SAT_STREAK

START=$(date +%s); DEADLINE=$((START + RUNTIME))
log(){ echo "[$(date -u +%FT%TZ)] $*" >> "$STATUS"; }

count_t(){ ps -eo comm= | awk -v c="${COMM[$1]}" '$1==c' | wc -l; }
newest_log(){ ls -t "$OUT/${1}".w*.log 2>/dev/null | head -1; }
cur_ft(){ local f v; f=$(newest_log "$1"); [ -z "$f" ] && { echo 0; return; }; v=$(grep -hoE 'ft: [0-9]+' "$f" | grep -oE '[0-9]+' | tail -1); echo "${v:-0}"; }
launch_t(){
  local t=$1 rem; rem=$((DEADLINE - $(date +%s))); [ "$rem" -lt 120 ] && rem=120
  setsid nohup nice -n 19 cargo +nightly fuzz run "$t" -- \
    -max_total_time="$rem" -rss_limit_mb=$RSS -seed="$RANDOM" -print_final_stats=1 \
    > "$OUT/${t}.w$(date +%s%N).log" 2>&1 &
}
kill_t_n(){ # kill the $2 OLDEST workers of target $1 (by elapsed time)
  local t=$1 n=$2 pids
  pids=$(ps -eo pid=,etimes=,comm= | awk -v c="${COMM[$t]}" '$3==c{print $2,$1}' | sort -rn | head -n "$n" | awk '{print $2}')
  [ -n "$pids" ] && kill $pids 2>/dev/null || true
}
set_target(){ # ensure target $1 has exactly $2 workers
  local t=$1 want=$2 cur; cur=$(count_t "$t")
  if   [ "$cur" -lt "$want" ]; then local i; for ((i=cur;i<want;i++)); do launch_t "$t"; done
  elif [ "$cur" -gt "$want" ]; then kill_t_n "$t" $((cur-want)); fi
}
desired(){ # echo "t:n ..." — split POOL across non-retired PRIORITY targets; soak if none
  local prog=() t; for t in "${PRIORITY[@]}"; do [ -n "${RETIRED[$t]:-}" ] || prog+=("$t"); done
  declare -A A; for t in "${ALL[@]}"; do A[$t]=0; done
  if   [ "${#prog[@]}" -eq 0 ]; then A[quic_packet]=1; A[osc_accumulation]=1
  elif [ "${#prog[@]}" -eq 1 ]; then A[${prog[0]}]=$POOL
  else A[${prog[0]}]=$(((POOL+1)/2)); A[${prog[1]}]=$((POOL/2)); fi
  local out=""; for t in "${ALL[@]}"; do out+="$t:${A[$t]} "; done; echo "$out"
}
reconcile(){ local kv; for kv in $1; do set_target "${kv%%:*}" "${kv##*:}"; done; }

# ---- main ----
find "$ROOT/fuzz/artifacts" -type f 2>/dev/null | sort > "$OUT/crashes.baseline"
for t in "${PRIORITY[@]}"; do cargo +nightly fuzz build "$t" >>"$OUT/build.log" 2>&1; done
log "ADAPTIVE start: decoders pre-retired (saturated over prior ~3h); pool=$POOL on quic/osc; deadline +$((RUNTIME/3600))h"
DA=$(desired); reconcile "$DA"; log "initial alloc -> $DA"

while true; do
  sleep 3600
  now=$(date +%s); el=$((now-START))

  # 1. crash check (highest priority)
  find "$ROOT/fuzz/artifacts" -type f 2>/dev/null | sort > "$OUT/crashes.now"
  nc=$(comm -13 "$OUT/crashes.baseline" "$OUT/crashes.now")
  if [ -n "$nc" ]; then
    log "!!! CRASH ARTIFACT — stopping all workers for triage"
    { echo CRASH; echo "$nc"; } > "$RESULT"
    for t in "${ALL[@]}"; do kill_t_n "$t" 99; done
    exit 2
  fi

  # 2. saturation accounting for active priority targets
  for t in "${PRIORITY[@]}"; do
    [ -n "${RETIRED[$t]:-}" ] && continue
    f=$(cur_ft "$t"); pf=${FT_LAST[$t]:-0}; d=$((f-pf)); FT_LAST[$t]=$f
    if [ "$d" -lt "$FT_THRESH" ]; then SAT_STREAK[$t]=$(( ${SAT_STREAK[$t]:-0} + 1 )); else SAT_STREAK[$t]=0; fi
    if [ "${SAT_STREAK[$t]:-0}" -ge "$SAT_STREAK_NEEDED" ]; then RETIRED[$t]=1; log "RETIRE $t (ft +$d last hour, flat ${SAT_STREAK[$t]}h)"; fi
  done

  # 3. snapshot
  { printf '[%s] hour %d alive:' "$(date -u +%FT%TZ)" "$((el/3600))"
    for t in "${ALL[@]}"; do printf ' %s=%s' "$t" "$(count_t "$t")"; done; echo
    for t in "${ALL[@]}"; do f=$(newest_log "$t"); line=$(grep -hE '^#[0-9]+' "$f" 2>/dev/null | tail -1); echo "    $t: ${line:-<retired/idle>}"; done
  } >> "$STATUS"

  # 4. rebalance
  DA=$(desired); reconcile "$DA"; log "alloc -> $DA"

  # 5. exits
  if [ "$now" -ge "$DEADLINE" ]; then
    log "deadline reached — stopping"; echo CLEAN > "$RESULT"
    for t in "${ALL[@]}"; do kill_t_n "$t" 99; done; exit 0
  fi
  tot=0; for t in "${ALL[@]}"; do tot=$((tot+$(count_t "$t"))); done
  if [ "$tot" -eq 0 ]; then log "STALL: no workers alive before deadline"; echo STALL > "$RESULT"; exit 0; fi
done
