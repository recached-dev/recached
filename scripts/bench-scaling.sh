#!/usr/bin/env bash
#
# Thread-scaling benchmark: does Recached actually use more than one core?
#
# A single throughput number cannot answer that. "Recached does 546k GET/sec"
# is equally consistent with a well-optimised single-threaded server, so it is
# not evidence of parallelism. What *is* evidence: hold the machine, the
# binary and the workload fixed, change only how many threads may execute
# commands, and see whether throughput moves.
#
# That is what this does. RECACHED_WORKER_THREADS is the only variable.
#
# Runs anywhere with a Recached binary and redis-benchmark — no Docker needed,
# which makes it the script to reach for on a real server. On Linux, set
# PIN=1 to taskset the server and the load generator onto disjoint cores;
# without that the load generator competes with the server for CPU and flattens
# the curve, understating the effect.
#
# Usage:
#   scripts/bench-scaling.sh
#   THREADS="1 2 4 8" N=200000 scripts/bench-scaling.sh
#   PIN=1 SERVER_CPUS=0-3 BENCH_CPUS=4-7 scripts/bench-scaling.sh   # Linux only

set -euo pipefail

BIN=${BIN:-./target/release/recached-server}
PORT=${PORT:-6395}
THREADS=${THREADS:-"1 2 4 8"}
N=${N:-100000}
CLIENTS=${CLIENTS:-50}
DATA=${DATA:-64}
KEYSPACE=${KEYSPACE:-100000}
PIPELINE=${PIPELINE:-16}
TESTS=${TESTS:-set,get,incr,lpush,sadd,hset,zadd}
OUT=${OUT:-bench-results}

PIN=${PIN:-0}
SERVER_CPUS=${SERVER_CPUS:-0-3}
BENCH_CPUS=${BENCH_CPUS:-4-7}

[[ -x $BIN ]] || { echo "no binary at $BIN — cargo build --release" >&2; exit 1; }
command -v redis-benchmark >/dev/null || { echo "redis-benchmark not found" >&2; exit 1; }

if [[ $PIN == 1 ]] && ! command -v taskset >/dev/null; then
  echo "PIN=1 needs taskset (Linux); continuing unpinned" >&2
  PIN=0
fi

mkdir -p "$OUT"
SERVER_PID=""
cleanup() { [[ -n $SERVER_PID ]] && kill -9 "$SERVER_PID" 2>/dev/null; return 0; }
trap cleanup EXIT

start_server() {
  local threads=$1
  local -a cmd=()
  [[ $PIN == 1 ]] && cmd+=(taskset -c "$SERVER_CPUS")
  cmd+=("$BIN")

  RECACHED_WORKER_THREADS="$threads" \
  RECACHED_BIND=127.0.0.1 \
  RECACHED_SAVE_INTERVAL=0 \
  RECACHED_PORT="$PORT" \
  RECACHED_WS_PORT=$((PORT + 1)) \
  RECACHED_METRICS_PORT=0 \
    "${cmd[@]}" >"$OUT/server-t${threads}.log" 2>&1 &
  SERVER_PID=$!

  local tries=40
  while (( tries-- > 0 )); do
    redis-cli -p "$PORT" ping 2>/dev/null | grep -q PONG && break
    sleep 0.25
  done

  # The run is meaningless if the knob was ignored, so confirm the server
  # built the runtime that was asked for rather than trusting the variable.
  local got
  got=$(sed -n 's/.*Command execution: \([0-9]*\) worker threads.*/\1/p' \
        "$OUT/server-t${threads}.log" | head -1)
  if [[ "$got" != "$threads" ]]; then
    echo "ERROR: asked for $threads worker threads, server reports '${got:-unknown}'" >&2
    exit 1
  fi
}

stop_server() {
  if [[ -n $SERVER_PID ]]; then
    # `disown` first: without it the shell prints its own "Killed: 9" job
    # notice to stderr, which lands in the middle of the results table.
    disown "$SERVER_PID" 2>/dev/null || true
    kill -9 "$SERVER_PID" 2>/dev/null || true
  fi
  SERVER_PID=""
  sleep 0.5
}

bench() {
  local -a pre=()
  [[ $PIN == 1 ]] && pre=(taskset -c "$BENCH_CPUS")
  # ${a[@]+"${a[@]}"} — expanding an empty array is an error under `set -u`
  # on bash 3.2, which is what macOS still ships.
  ${pre[@]+"${pre[@]}"} redis-benchmark -p "$PORT" -t "$TESTS" -n "$N" -c "$CLIENTS" \
    -d "$DATA" -r "$KEYSPACE" -P "$PIPELINE" --csv
}

echo "# binary:   $BIN"
echo "# workload: -n $N -c $CLIENTS -d $DATA -r $KEYSPACE -P $PIPELINE"
echo "# pinning:  $([[ $PIN == 1 ]] && echo "server=$SERVER_CPUS bench=$BENCH_CPUS" || echo "none (load generator shares cores with the server)")"
echo

for t in $THREADS; do
  redis-cli -p "$PORT" shutdown nosave >/dev/null 2>&1 || true
  start_server "$t"

  # FLUSHDB, not FLUSHALL: Recached is a single-database server and does not
  # implement FLUSHALL. redis-cli exits 0 even when the server answers -ERR,
  # so the wrong verb here does not fail the run — it silently leaves the
  # warm-up's keys in place and every measurement below is taken against a
  # keyspace that was never reset.
  redis-cli -p "$PORT" flushdb >/dev/null
  redis-benchmark -p "$PORT" -t set,get -n 10000 -c "$CLIENTS" -d "$DATA" -q >/dev/null
  redis-cli -p "$PORT" flushdb >/dev/null

  echo "### worker threads = $t"
  bench | tee "$OUT/scaling-t${t}.csv"
  echo

  stop_server
done

echo "# raw CSV in $OUT/scaling-t*.csv"
