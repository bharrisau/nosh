#!/usr/bin/env bash
# Live snapshot of the (adaptive) overnight fuzz run — safe anytime, no LLM needed.
#   bash fuzz/overnight/status.sh
ROOT=/home/bharris/github.com/bharrisau/nosh
OUT="$ROOT/fuzz/overnight"
declare -A COMM=( [quic_packet]=quic_packet [osc_accumulation]=osc_accumulatio [codec_decode]=codec_decode [read_message]=read_message [decode_datagram]=decode_datagram [decode_epoch_ack]=decode_epoch_ac )
echo "== RESULT marker =="; { [ -s "$OUT/RESULT" ] && cat "$OUT/RESULT"; } || echo "(empty — still running)"
echo
echo "== driver =="; p=$(cat "$OUT/driver.pid" 2>/dev/null); { [ -n "$p" ] && kill -0 "$p" 2>/dev/null && echo "alive (pid $p)"; } || echo "NOT running"
echo
echo "== live workers (by process name) + crashes =="
tot=0
for t in quic_packet osc_accumulation codec_decode read_message decode_datagram decode_epoch_ack; do
  c=$(ps -eo comm= | awk -v x="${COMM[$t]}" '$1==x' | wc -l); tot=$((tot+c))
  printf '  %-18s %s\n' "$t" "$c"
done
echo "  TOTAL: $tot"
find "$ROOT/fuzz/artifacts" -type f 2>/dev/null | sort > /tmp/_cn
echo "  new crash artifacts: $(comm -13 "$OUT/crashes.baseline" /tmp/_cn 2>/dev/null | grep -c . || echo 0)"
echo
echo "== latest progress per target (newest log) =="
for t in quic_packet osc_accumulation codec_decode read_message decode_datagram decode_epoch_ack; do
  f=$(ls -t "$OUT/${t}".w*.log 2>/dev/null | head -1)
  line=$(grep -hE '^#[0-9]+' "$f" 2>/dev/null | tail -1)
  printf '  %-18s %s\n' "$t" "${line:-<retired/idle>}"
done
echo
echo "== last rebalance decisions =="; grep -E "RETIRE|alloc ->|ADAPTIVE start" "$OUT/status.txt" 2>/dev/null | tail -6
