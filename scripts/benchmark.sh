#!/usr/bin/env bash
# Benchmark recached against Redis / Valkey with redis-benchmark.
#
# Usage:
#   scripts/benchmark.sh                 # benchmark recached on 127.0.0.1:6379
#   PORT=6390 scripts/benchmark.sh       # benchmark whatever listens on 6390
#
# Start each server yourself, one at a time, with persistence disabled:
#   RECACHED_BIND=127.0.0.1 RECACHED_SAVE_INTERVAL=0 recached-server
#   redis-server  --port 6390 --bind 127.0.0.1 --save '' --appendonly no
#   valkey-server --port 6391 --bind 127.0.0.1 --save '' --appendonly no
#
# Results used in docs/guide/benchmarks.md were produced with this script.
set -euo pipefail

PORT=${PORT:-6379}
N=${N:-100000}          # requests per test
CLIENTS=${CLIENTS:-50}  # parallel connections
DATA=${DATA:-64}        # value size in bytes
KEYSPACE=${KEYSPACE:-100000}
TESTS="set,get,incr,lpush,rpop,sadd,hset,spop,zadd,lrange,mset"

command -v redis-benchmark >/dev/null || { echo "redis-benchmark not found" >&2; exit 1; }
redis-cli -p "$PORT" ping >/dev/null || { echo "no server on port $PORT" >&2; exit 1; }

echo "# server on port $PORT — $(redis-cli -p "$PORT" info server 2>/dev/null | grep -E 'redis_version|valkey_version' | head -1 || echo 'recached (INFO subset)')"

redis-cli -p "$PORT" flushdb >/dev/null

echo "# warm-up"
redis-benchmark -p "$PORT" -t set,get -n 10000 -c "$CLIENTS" -d "$DATA" -q >/dev/null
redis-cli -p "$PORT" flushdb >/dev/null

echo "# main run (no pipelining)"
redis-benchmark -p "$PORT" -t "$TESTS" -n "$N" -c "$CLIENTS" -d "$DATA" -r "$KEYSPACE" --csv
redis-cli -p "$PORT" flushdb >/dev/null

echo "# pipelined run (P=16)"
redis-benchmark -p "$PORT" -t set,get,incr,lpush,sadd,hset,zadd -n "$N" -c "$CLIENTS" -d "$DATA" -r "$KEYSPACE" -P 16 --csv
redis-cli -p "$PORT" flushdb >/dev/null
