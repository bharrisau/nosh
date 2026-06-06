#!/usr/bin/env bash
# 999.1 overnight deep-fuzz driver.
# Launches parallel libFuzzer workers across all six targets for ~18h, sharing
# each target's corpus. Self-monitors hourly into status.txt. Exits early ONLY
# on a new crash artifact or an unexpected worker death (stall); otherwise runs
# to completion. The controlling (run_in_background) shell is notified on exit.
set -u

ROOT=/home/bharris/github.com/bharrisau/nosh
cd "$ROOT" || exit 99
OUT="$ROOT/fuzz/overnight"
mkdir -p "$OUT"
RESULT="$OUT/RESULT"
STATUS="$OUT/status.txt"
: > "$RESULT"

DUR=64800          # 18h per worker (libFuzzer -max_total_time, seconds)
GRACE=900          # extra wall-clock slack before we call a missing worker "done"
RSS_MB=2048        # default libFuzzer RSS cap — an OOM here is a REAL finding (unbounded growth)

# target:worker-count  (sum = 9 — HALVED from 18; the box has a large non-nosh baseline load).
# Workers also run at idle priority (nice -n 19) so they yield to other contending work.
TARGETS=(quic_packet osc_accumulation codec_decode read_message decode_datagram decode_epoch_ack)
declare -A ALLOC=( [quic_packet]=3 [osc_accumulation]=2 [codec_decode]=1 [read_message]=1 [decode_datagram]=1 [decode_epoch_ack]=1 )

log() { echo "[$(date -u +%FT%TZ)] $*" >> "$STATUS"; }

log "=== 999.1 overnight fuzz START — 18h target, $(nproc) cores ==="

# 1. Build all six targets FIRST (avoid concurrent build races when workers launch).
for t in "${TARGETS[@]}"; do
  log "building $t ..."
  if ! cargo +nightly fuzz build "$t" >> "$OUT/build.log" 2>&1; then
    log "FATAL: build failed for $t — see build.log"
    echo "BUILD_FAIL $t" > "$RESULT"
    exit 1
  fi
done
log "all six targets built OK"

# 2. Baseline existing crash artifacts so we only react to NEW ones.
find "$ROOT/fuzz/artifacts" -type f 2>/dev/null | sort > "$OUT/crashes.baseline"

# 3. Launch workers. Each shares its target's corpus dir; distinct -seed per worker.
: > "$OUT/pids.txt"
for t in "${TARGETS[@]}"; do
  n=${ALLOC[$t]}
  for i in $(seq 1 "$n"); do
    nohup nice -n 19 cargo +nightly fuzz run "$t" -- \
      -max_total_time=$DUR -rss_limit_mb=$RSS_MB -seed="$i" -print_final_stats=1 \
      > "$OUT/${t}.w${i}.log" 2>&1 &
    echo "$! $t w$i" >> "$OUT/pids.txt"
  done
done
NWORKERS=$(wc -l < "$OUT/pids.txt")
START=$(date +%s)
log "launched $NWORKERS workers at idle priority (quic=3 osc=2 codec=1 read=1 dgram=1 ack=1)"

# 4. Hourly self-monitor loop.
HOUR=0
while true; do
  sleep 3600
  HOUR=$((HOUR+1))
  NOW=$(date +%s); ELAPSED=$((NOW-START))

  # alive worker count (cargo parent PIDs from pids.txt)
  alive=0
  while read -r pid _; do kill -0 "$pid" 2>/dev/null && alive=$((alive+1)); done < "$OUT/pids.txt"

  # new crash artifacts?
  find "$ROOT/fuzz/artifacts" -type f 2>/dev/null | sort > "$OUT/crashes.now"
  newcrash=$(comm -13 "$OUT/crashes.baseline" "$OUT/crashes.now")

  # per-target latest libFuzzer progress line (cov / ft / exec/s)
  {
    echo "[$(date -u +%FT%TZ)] hour $HOUR  (elapsed ${ELAPSED}s)  alive=$alive/$NWORKERS"
    for t in "${TARGETS[@]}"; do
      line=$(grep -hE '^#[0-9]+' "$OUT/${t}.w"*.log 2>/dev/null | tail -1)
      echo "    $t: ${line:-<no progress line yet>}"
    done
  } >> "$STATUS"

  if [ -n "$newcrash" ]; then
    log "!!! CRASH ARTIFACT DETECTED — stopping all workers for triage"
    { echo "CRASH"; echo "$newcrash"; } > "$RESULT"
    while read -r pid _; do kill "$pid" 2>/dev/null; done < "$OUT/pids.txt"
    pkill -f 'cargo.*fuzz run' 2>/dev/null
    exit 2
  fi

  if [ "$alive" -eq 0 ]; then
    if [ "$ELAPSED" -ge $((DUR - 300)) ]; then
      log "all workers finished cleanly at the 18h deadline"
      echo "CLEAN" > "$RESULT"
    else
      log "STALL: all workers died at ${ELAPSED}s (< 18h) with no crash artifact — investigate"
      echo "STALL" > "$RESULT"
    fi
    exit 0
  fi

  # hard safety stop well past the deadline
  if [ "$ELAPSED" -ge $((DUR + GRACE)) ]; then
    log "deadline+grace reached; stopping remaining workers"
    while read -r pid _; do kill "$pid" 2>/dev/null; done < "$OUT/pids.txt"
    echo "CLEAN" > "$RESULT"
    exit 0
  fi
done
