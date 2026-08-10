#!/bin/sh
# cml end-to-end query benchmark.
#
# Method mirrors nula/BENCH.md: median-of-N, own runner, no external harness.
# Measures the SHIPPED Release binary end to end (process start -> results printed),
# because that is the latency a caller actually pays.
#
# metal: shell rather than a compiled language — it times an external binary and prints
# a markdown row; there is no logic here worth making deterministic.
#
# HARD RULE, learned the expensive way: never time a debug build. `cargo build` without
# --release is unoptimized and nowhere near what ships. The C++ tree went further and
# made its Debug profile the sanitizer build (~10x slower); benchmarking that against a
# Release predecessor once produced a 6.6x gap that did not exist. The sanitizer check
# below is kept for the same reason it was written: the most misleading measurement
# available is a correct method pointed at the wrong binary.

set -eu

BIN="${CML_BIN:-$HOME/.local/bin/cml}"
REPS="${REPS:-9}"
LIMIT="${LIMIT:-20}"

[ -x "$BIN" ] || { echo "no cml binary at $BIN" >&2; exit 1; }

# Refuse an instrumented binary — the single most misleading measurement available.
if nm -C "$BIN" 2>/dev/null | grep -q '__asan_\|__ubsan_'; then
  echo "REFUSING: $BIN is sanitizer-instrumented; benchmark Release only" >&2
  exit 2
fi

# Host contention makes every number below meaningless. nula's BENCH.md records this
# honestly rather than hiding it; so do we.
LOAD=$(cut -d' ' -f1 /proc/loadavg)
CORES=$(nproc)
CONTENDED=$(awk -v l="$LOAD" -v c="$CORES" 'BEGIN{print (l > c/2) ? "yes" : "no"}')

median() {  # stdin: one number per line
  sort -n | awk '{a[NR]=$1} END{ if(NR%2) print a[(NR+1)/2]; else print (a[NR/2]+a[NR/2+1])/2 }'
}

time_one() {  # $1=query -> milliseconds, one run
  s=$(date +%s%N)
  "$BIN" search "$1" --keyword --limit "$LIMIT" >/dev/null 2>&1 || true
  e=$(date +%s%N)
  echo "scale=2; ($e - $s) / 1000000" | bc
}

run_query() {  # $1=query -> median ms over REPS, after one warm-up
  time_one "$1" >/dev/null
  i=0
  while [ "$i" -lt "$REPS" ]; do time_one "$1"; i=$((i+1)); done | median
}

INDEX="$HOME/.claude/claude-memory-light/index.db"
SIZE=$(stat -c%s "$INDEX" 2>/dev/null || echo 0)

echo "# cml bench $(date +%F) — index $((SIZE/1024/1024)) MB, median of $REPS, limit $LIMIT"
echo "# host: $CORES cores, load $LOAD, contended=$CONTENDED"
[ "$CONTENDED" = yes ] && echo "# WARNING: host is loaded; these numbers are not comparable"
echo
echo "| date | benchmark | result |"
echo "|---|---|---|"
for q in "touchpad drag" "sanitizer build" "embedding leg recall" "rust c++ comparison"; do
  ms=$(run_query "$q")
  echo "| $(date +%F) | \`search --keyword\` \"$q\", limit $LIMIT | ${ms} ms |"
done
