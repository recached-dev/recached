#!/usr/bin/env bash
#
# Linux benchmark harness for Recached, Redis and Valkey — all in containers.
#
# Why this exists alongside scripts/benchmark.sh: that script benchmarks
# whatever is already listening on a port, on whatever machine you run it on.
# The numbers it produced for docs/guide/benchmarks.md came from a 4-core
# macOS laptop with the load generator competing with the server for the same
# cores, which is the single largest caveat on those figures.
#
# This harness fixes three things:
#
#   1. Linux, reproducibly. Every server runs from a pinned image, so the
#      kernel, allocator and libc are the same for all three and the run can
#      be repeated on any machine with Docker.
#   2. The load generator gets its own cores. The server is pinned to
#      SERVER_CPUS and redis-benchmark to BENCH_CPUS, and the two sets must be
#      disjoint. Without this, a server that uses more cores is penalised by
#      starving the very tool that measures it — which biases the result
#      against the multi-threaded server specifically.
#   3. It measures thread scaling, not just throughput. Recached is run at
#      several worker-thread counts on an unchanged CPU set, so the only
#      variable is how many threads execute commands. That curve is the
#      evidence for the multi-threading claim; a single throughput number
#      is not, because it cannot be told apart from a faster hot loop.
#
# The load generator shares the server's network namespace, so traffic is
# loopback rather than a bridge hop — the same shape as the original localhost
# runs, applied identically to all three servers.
#
# Usage:
#   scripts/bench-docker.sh                 # full run
#   scripts/bench-docker.sh --quick         # 20k requests, for a smoke test
#   SERVER_CPUS=0-5 BENCH_CPUS=6-7 scripts/bench-docker.sh
#
# Results land in $OUT (default bench-results/) as CSV files plus conditions.txt.

set -euo pipefail

# ── configuration ────────────────────────────────────────────────────────────

IMAGE=${IMAGE:-recached-bench:local}
REDIS_IMAGE=${REDIS_IMAGE:-redis:7.2.5-bookworm}
VALKEY_IMAGE=${VALKEY_IMAGE:-valkey/valkey:9.1.2-trixie}

# Disjoint CPU sets. The defaults assume at least 8 CPUs; on a smaller machine
# override both, but keep them disjoint or the comparison is meaningless.
SERVER_CPUS=${SERVER_CPUS:-0-3}
BENCH_CPUS=${BENCH_CPUS:-4-7}

N=${N:-100000}            # requests per test
CLIENTS=${CLIENTS:-50}    # parallel connections
DATA=${DATA:-64}          # value size in bytes
KEYSPACE=${KEYSPACE:-100000}
PIPELINE=${PIPELINE:-16}
MEMORY_KEYS=${MEMORY_KEYS:-100000}
MEMORY_DATA=${MEMORY_DATA:-64}

# Worker-thread counts for the scaling run. Keep the largest at or below the
# number of cores in SERVER_CPUS: past that the curve measures oversubscription
# rather than parallelism, which is a different (and less interesting) claim.
SCALE_THREADS=${SCALE_THREADS:-"1 2 4"}

# Commands worth comparing. LRANGE and the SPOP/RPOP family are excluded from
# the pipelined set for the same reason the existing docs exclude them: their
# cost is dominated by reply construction, not by command dispatch.
PIPELINED_TESTS=${PIPELINED_TESTS:-set,get,incr,lpush,sadd,hset,zadd}
UNPIPELINED_TESTS=${UNPIPELINED_TESTS:-set,get,incr,lpush,rpop,sadd,hset,spop,zadd,lrange,mset}

OUT=${OUT:-bench-results}

if [[ "${1:-}" == "--quick" ]]; then
  N=20000
  SCALE_THREADS="1 4"
  echo "# quick mode: N=$N, threads='$SCALE_THREADS'"
fi

# ── preflight ────────────────────────────────────────────────────────────────

command -v docker >/dev/null || { echo "docker not found" >&2; exit 1; }
docker image inspect "$IMAGE" >/dev/null 2>&1 || {
  echo "image '$IMAGE' not found — build it first:" >&2
  echo "  docker build -t $IMAGE ." >&2
  exit 1
}

# Expand a cpuset spec ("0-3", "0,2,4") into a count, so the script can warn
# when SCALE_THREADS asks for more threads than there are cores to run them on.
cpuset_count() {
  local spec=$1 total=0 part lo hi
  IFS=',' read -ra parts <<<"$spec"
  for part in "${parts[@]}"; do
    if [[ $part == *-* ]]; then
      lo=${part%-*}; hi=${part#*-}
      total=$(( total + hi - lo + 1 ))
    else
      total=$(( total + 1 ))
    fi
  done
  echo "$total"
}

SERVER_CORES=$(cpuset_count "$SERVER_CPUS")
BENCH_CORES=$(cpuset_count "$BENCH_CPUS")
HOST_CPUS=$(docker info --format '{{.NCPU}}')

# Docker Desktop for Mac and Windows runs the engine inside a VM whose network
# is emulated in userspace (gVisor). That adds roughly 0.8 ms to every
# round-trip, against ~0.05 ms on a native loopback. Pipelining amortises it
# over 16 commands and survives; an unpipelined test does one round-trip per
# command, so its ceiling becomes ~1/0.0008 = 1250 rps per connection and the
# result measures the VM's network stack rather than any server. Measured with
# `redis-cli --latency` inside the VM — check it yourself before trusting an
# unpipelined number from this harness.
if [[ "$(uname -s)" != "Linux" ]]; then
  echo "WARNING: Docker Desktop on $(uname -s) emulates the network in userspace (~0.8ms/round-trip)." >&2
  echo "         Pipelined and thread-scaling results are meaningful; UNPIPELINED ONES ARE NOT." >&2
  echo "         For publishable unpipelined figures, run this on a native Linux host." >&2
fi

echo "# host: $HOST_CPUS CPUs visible to Docker"
echo "# server pinned to CPUs $SERVER_CPUS ($SERVER_CORES cores)"
echo "# bench  pinned to CPUs $BENCH_CPUS ($BENCH_CORES cores)"

if (( SERVER_CORES + BENCH_CORES > HOST_CPUS )); then
  echo "WARNING: the two CPU sets need $((SERVER_CORES + BENCH_CORES)) cores but Docker sees $HOST_CPUS." >&2
  echo "         They are probably overlapping, which invalidates the comparison." >&2
fi

mkdir -p "$OUT"

# ── container lifecycle ──────────────────────────────────────────────────────

CONTAINERS=()

cleanup() {
  if (( ${#CONTAINERS[@]} )); then
    docker rm -f "${CONTAINERS[@]}" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

# Start a server container pinned to SERVER_CPUS with persistence disabled.
# Persistence is off everywhere: a background save mid-run shows up as a
# throughput cliff on whichever server happens to be measured when it fires.
start_recached() {
  local name=$1 threads=${2:-}
  local -a env_args=(-e RECACHED_SAVE_INTERVAL=0 -e RECACHED_BIND=0.0.0.0)
  [[ -n $threads ]] && env_args+=(-e "RECACHED_WORKER_THREADS=$threads")
  docker run -d --name "$name" --cpuset-cpus="$SERVER_CPUS" \
    "${env_args[@]}" "$IMAGE" >/dev/null
  CONTAINERS+=("$name")
}

start_redis() {
  local name=$1
  docker run -d --name "$name" --cpuset-cpus="$SERVER_CPUS" "$REDIS_IMAGE" \
    redis-server --save '' --appendonly no --protected-mode no >/dev/null
  CONTAINERS+=("$name")
}

start_valkey() {
  local name=$1
  docker run -d --name "$name" --cpuset-cpus="$SERVER_CPUS" "$VALKEY_IMAGE" \
    valkey-server --save '' --appendonly no --protected-mode no >/dev/null
  CONTAINERS+=("$name")
}

# Run a client tool inside the server's network namespace, on the bench cores.
in_bench_ns() {
  local server=$1; shift
  docker run --rm --cpuset-cpus="$BENCH_CPUS" --network "container:$server" \
    "$REDIS_IMAGE" "$@"
}

wait_ready() {
  local server=$1 tries=60
  while (( tries-- > 0 )); do
    if in_bench_ns "$server" redis-cli -p 6379 ping 2>/dev/null | grep -q PONG; then
      return 0
    fi
    sleep 0.5
  done
  echo "server '$server' never answered PING" >&2
  docker logs "$server" >&2 || true
  return 1
}

# Read back the worker count the server actually built. The whole scaling run
# is worthless if RECACHED_WORKER_THREADS was ignored, so this is asserted
# rather than assumed.
observed_workers() {
  docker logs "$1" 2>&1 | sed -n 's/.*Command execution: \([0-9]*\) worker threads.*/\1/p' | head -1
}

# ── measurement ──────────────────────────────────────────────────────────────

# redis-benchmark, warmed up and against a clean keyspace, to a CSV file.
measure() {
  local server=$1 tests=$2 pipeline=$3 outfile=$4

  in_bench_ns "$server" redis-cli -p 6379 flushdb >/dev/null
  in_bench_ns "$server" redis-benchmark -p 6379 -t set,get -n 10000 \
    -c "$CLIENTS" -d "$DATA" -q >/dev/null
  in_bench_ns "$server" redis-cli -p 6379 flushdb >/dev/null

  local -a args=(-p 6379 -t "$tests" -n "$N" -c "$CLIENTS" -d "$DATA" -r "$KEYSPACE" --csv)
  (( pipeline > 1 )) && args+=(-P "$pipeline")

  in_bench_ns "$server" redis-benchmark "${args[@]}" >"$outfile"
  in_bench_ns "$server" redis-cli -p 6379 flushdb >/dev/null
}

# ── run 1: thread scaling (the multi-threading proof) ────────────────────────
#
# One server image, one CPU set, one workload. The only thing that changes is
# how many threads are allowed to execute commands.

echo
echo "## thread scaling — Recached, CPUs $SERVER_CPUS fixed, pipelined P=$PIPELINE"

: >"$OUT/scaling-raw.txt"
for t in $SCALE_THREADS; do
  if (( t > SERVER_CORES )); then
    echo "# note: $t threads on $SERVER_CORES cores — oversubscribed"
  fi
  name="bench-recached-t$t"
  docker rm -f "$name" >/dev/null 2>&1 || true
  start_recached "$name" "$t"
  wait_ready "$name"

  got=$(observed_workers "$name")
  if [[ "$got" != "$t" ]]; then
    echo "ERROR: asked for $t worker threads, server reports '${got:-unknown}'" >&2
    exit 1
  fi
  echo "# recached: $t worker threads (confirmed by the server)"

  measure "$name" "$PIPELINED_TESTS" "$PIPELINE" "$OUT/scaling-p${PIPELINE}-t${t}.csv"
  { echo "### threads=$t"; cat "$OUT/scaling-p${PIPELINE}-t${t}.csv"; echo; } \
    >>"$OUT/scaling-raw.txt"
  cat "$OUT/scaling-p${PIPELINE}-t${t}.csv"

  docker rm -f "$name" >/dev/null 2>&1 || true
done

# ── run 2: three-way comparison ──────────────────────────────────────────────

echo
echo "## three-way — Recached (all $SERVER_CORES cores) vs Redis vs Valkey"

run_threeway() {
  local label=$1 name=$2
  wait_ready "$name"
  echo "# $label — pipelined P=$PIPELINE"
  measure "$name" "$PIPELINED_TESTS" "$PIPELINE" "$OUT/threeway-p${PIPELINE}-${label}.csv"
  cat "$OUT/threeway-p${PIPELINE}-${label}.csv"
  echo "# $label — unpipelined"
  measure "$name" "$UNPIPELINED_TESTS" 1 "$OUT/threeway-p1-${label}.csv"
  cat "$OUT/threeway-p1-${label}.csv"
  docker rm -f "$name" >/dev/null 2>&1 || true
}

docker rm -f bench-recached >/dev/null 2>&1 || true
start_recached bench-recached ""
echo "# recached workers: $(wait_ready bench-recached && observed_workers bench-recached)"
run_threeway recached bench-recached

docker rm -f bench-redis >/dev/null 2>&1 || true
start_redis bench-redis
run_threeway redis bench-redis

if docker image inspect "$VALKEY_IMAGE" >/dev/null 2>&1; then
  docker rm -f bench-valkey >/dev/null 2>&1 || true
  start_valkey bench-valkey
  run_threeway valkey bench-valkey
else
  echo "# valkey image '$VALKEY_IMAGE' not present — skipping" >&2
fi

# ── run 3: memory per live key ───────────────────────────────────────────────
#
# Measure process RSS before and after loading each data shape. The delta is
# divided by DBSIZE rather than the requested count because randomized keys can
# collide. Linux /proc reports bytes without Docker's human-unit rounding.

container_rss_kb() {
  local name=$1 pid
  pid=$(docker inspect -f '{{.State.Pid}}' "$name")
  awk '/^VmRSS:/ { print $2 }' "/proc/$pid/status"
}

measure_memory() {
  local label=$1 name=$2 test=$3 outfile=$4 baseline loaded keys delta bytes_per_key
  wait_ready "$name"
  in_bench_ns "$name" redis-cli -p 6379 flushdb >/dev/null
  sleep 1
  baseline=$(container_rss_kb "$name")
  in_bench_ns "$name" redis-benchmark -p 6379 -t "$test" -n "$MEMORY_KEYS" \
    -c "$CLIENTS" -d "$MEMORY_DATA" -r $((MEMORY_KEYS * 10)) -P "$PIPELINE" -q >/dev/null
  sleep 1
  keys=$(in_bench_ns "$name" redis-cli -p 6379 --raw dbsize)
  loaded=$(container_rss_kb "$name")
  delta=$(( (loaded - baseline) * 1024 ))
  bytes_per_key=$(awk -v bytes="$delta" -v count="$keys" \
    'BEGIN { if (count > 0) printf "%.2f", bytes / count; else print "n/a" }')
  printf '%s,%s,%s,%s,%s,%s\n' "$label" "$test" "$keys" "$baseline" "$loaded" "$bytes_per_key" >>"$outfile"
  in_bench_ns "$name" redis-cli -p 6379 flushdb >/dev/null
}

MEMORY_OUT="$OUT/memory-per-key.csv"
echo 'server,dataset,live_keys,baseline_rss_kb,loaded_rss_kb,delta_bytes_per_key' >"$MEMORY_OUT"

# Use a fresh process for every data shape. FLUSHDB removes keys but allocators
# retain arenas, so reusing one process would contaminate later RSS baselines.
for test in set hset sadd; do
  name="bench-memory-recached-$test"
  docker rm -f "$name" >/dev/null 2>&1 || true
  start_recached "$name" ""
  measure_memory recached "$name" "$test" "$MEMORY_OUT"
  docker rm -f "$name" >/dev/null 2>&1 || true
done

for test in set hset sadd; do
  name="bench-memory-redis-$test"
  docker rm -f "$name" >/dev/null 2>&1 || true
  start_redis "$name"
  measure_memory redis "$name" "$test" "$MEMORY_OUT"
  docker rm -f "$name" >/dev/null 2>&1 || true
done

if docker image inspect "$VALKEY_IMAGE" >/dev/null 2>&1; then
  for test in set hset sadd; do
    name="bench-memory-valkey-$test"
    docker rm -f "$name" >/dev/null 2>&1 || true
    start_valkey "$name"
    measure_memory valkey "$name" "$test" "$MEMORY_OUT"
    docker rm -f "$name" >/dev/null 2>&1 || true
  done
fi

cat "$MEMORY_OUT"

# ── provenance ───────────────────────────────────────────────────────────────
#
# A benchmark without its conditions is an anecdote. Everything needed to
# judge or repeat the run goes next to the numbers.
{
  echo "date:            $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "docker:          $(docker version --format '{{.Server.Version}}')"
  echo "docker cpus:     $HOST_CPUS"
  echo "docker kernel:   $(docker info --format '{{.KernelVersion}}')"
  echo "docker os/arch:  $(docker info --format '{{.OSType}}/{{.Architecture}}')"
  echo "recached image:  $IMAGE ($(docker image inspect -f '{{.Id}}' "$IMAGE"))"
  echo "redis image:     $REDIS_IMAGE"
  echo "valkey image:    $VALKEY_IMAGE"
  echo "server cpuset:   $SERVER_CPUS ($SERVER_CORES cores)"
  echo "bench cpuset:    $BENCH_CPUS ($BENCH_CORES cores)"
  echo "workload:        -n $N -c $CLIENTS -d $DATA -r $KEYSPACE"
  echo "pipeline:        $PIPELINE"
  echo "scale threads:   $SCALE_THREADS"
  echo "memory workload: $MEMORY_KEYS operations, $MEMORY_DATA-byte values, SET/HSET/SADD"
} >"$OUT/conditions.txt"

echo
echo "# done — results in $OUT/"
cat "$OUT/conditions.txt"
