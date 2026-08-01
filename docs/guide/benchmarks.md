# Benchmarks

How the Recached server compares to Redis 7.2.5 and Valkey 9.1.0 under `redis-benchmark`, measured July 2026 on Recached v0.1.8.

::: warning Measured on v0.1.8
These numbers predate v0.2.0, which added the exactly-once `DEDUP` envelope on every store write and extracted the sync client. Server-side command paths were not the target of those changes, but the suite has not been re-run since — treat the table as v0.1.8 evidence, not a current measurement. Redis 7.2.5 was also current when this ran; newer Redis releases may perform differently.
:::

::: tip TL;DR
Pipelined (`-P 16`), Recached sustains **408k–546k requests/sec** — ahead of Redis on 6 of 7 commands and ahead of Valkey on all 7, on the same 4-core machine. Unpipelined — one command per round-trip, the traffic shape of typical application cache calls — Recached runs at 46–96% of Redis with **sub-millisecond p50 latency on every single-key command** (multi-element `LRANGE` reads are the exception, at 1.96–3.58 ms p50).
:::

Recached's design goal is not to beat Redis at raw server throughput — it is to remove the network round-trip entirely for browser reads, which no server-side cache can do. These numbers cover the server half (`server-native`) so you know what to expect when you point existing Redis clients at it.

## Environment

| | |
|---|---|
| Hardware | Intel Core i5-8259U (4 cores / 8 threads, 2.3 GHz), 8 GB RAM |
| OS | macOS (Darwin 24.6.0) |
| Recached | v0.1.8, `cargo build --release` (thin LTO, jemalloc) |
| Redis | 7.2.5 (Homebrew) |
| Valkey | 9.1.0 (Homebrew) |
| Load generator | `redis-benchmark` from Redis 7.2.5 |

Methodology:

- Servers ran **one at a time** on localhost, with the load generator on the same machine.
- Persistence disabled everywhere: `RECACHED_SAVE_INTERVAL=0` for Recached; `--save '' --appendonly no` for Redis and Valkey.
- Each server got a 10k-request warm-up, then `FLUSHDB`, then the measured run: **100,000 requests, 50 parallel connections, 64-byte values, randomized keys** (`-n 100000 -c 50 -d 64 -r 100000`), tests back-to-back in suite order.
- Reproduce with [`scripts/benchmark.sh`](https://github.com/recached-dev/recached/blob/main/scripts/benchmark.sh).

One caveat: this is a 4-core laptop and the benchmark tool competes with the servers for cores — absolute numbers on server hardware will be higher for all three systems. Recached's multi-threaded runtime has the most headroom to gain from more cores; Redis and Valkey process commands on a single thread.

## Pipelined (`-P 16`)

Pipelining batches 16 commands per round-trip, measuring raw server-side command throughput rather than round-trip handling. Requests per second; **bold** marks the best result per row.

| Command | Recached rps | Redis rps | Valkey rps |
|---|---:|---:|---:|
| SET | **421,941** | 375,940 | 294,118 |
| GET | **546,448** | 512,821 | 483,092 |
| INCR | **448,430** | 421,941 | 413,223 |
| LPUSH | **473,934** | 409,836 | 386,100 |
| SADD | 421,941 | **462,963** | 378,788 |
| HSET | **408,163** | 324,675 | 287,356 |
| ZADD | **414,938** | 197,628 | 221,239 |

This is where multi-threading pays: Recached spreads 50 connections across all cores, while Redis and Valkey execute commands on one. p50 latency stays around 0.7–1.0 ms and p99 under 5.1 ms across the suite.

These numbers are new in v0.1.8. In v0.1.7, pipelined throughput collapsed after the first test of a run (INCR 13.5k, LPUSH 9.8k rps, with multi-second stalls). Profiling traced it to per-command costs that deep pipelines amplify — chiefly per-op metrics-registry lookups whose global-recorder contention across 8 worker threads caused the decay and the stalls, plus a full `Command` clone per execution and a fresh allocation per response. v0.1.8 caches the counter handles, moves the command instead of cloning it (cloning only when a WebSocket peer, replica, AOF, or watched key actually consumes the write), and serializes responses into a reused per-connection buffer.

## No pipelining (one command per round-trip)

Requests per second; p50/p99 latency in milliseconds.

| Command | Recached rps | Recached p50 / p99 | Redis rps | Redis p50 / p99 | Valkey rps | Valkey p50 / p99 |
|---|---:|---:|---:|---:|---:|---:|
| SET | 51,706 | 0.46 / 1.06 | 57,110 | 0.46 / 1.02 | 56,657 | 0.46 / 0.97 |
| GET | 58,072 | 0.44 / 0.66 | 61,576 | 0.42 / 0.64 | 60,569 | 0.42 / 0.84 |
| INCR | 52,549 | 0.46 / 1.05 | 61,805 | 0.42 / 0.60 | 59,737 | 0.43 / 0.87 |
| LPUSH | 49,116 | 0.52 / 0.84 | 61,843 | 0.42 / 0.55 | 61,425 | 0.43 / 0.70 |
| RPOP | 45,914 | 0.46 / 1.64 | 62,500 | 0.42 / 0.61 | 63,452 | 0.42 / 0.55 |
| SADD | 51,626 | 0.44 / 1.53 | 62,228 | 0.42 / 0.54 | 62,422 | 0.42 / 0.59 |
| HSET | 28,145 | 0.74 / 5.27 | 61,576 | 0.43 / 0.63 | 62,539 | 0.43 / 0.62 |
| SPOP | 33,772 | 0.63 / 4.71 | 62,972 | 0.42 / 0.58 | 63,654 | 0.43 / 0.88 |
| ZADD | 36,010 | 0.54 / 4.46 | 62,189 | 0.43 / 0.65 | 62,422 | 0.44 / 0.77 |
| MSET (10 keys) | 34,459 | 0.66 / 2.67 | 35,817 | 1.16 / 2.22 | 40,933 | 1.04 / 1.58 |
| LRANGE_100 | 11,663 | 1.96 / 7.69 | 18,406 | 1.34 / 1.75 | 18,567 | 1.34 / 1.63 |
| LRANGE_300 | 5,261 | 3.55 / 10.51 | 7,458 | 2.81 / 5.03 | 7,541 | 2.74 / 4.78 |
| LRANGE_500 | 3,457 | 3.43 / 8.87 | 4,393 | 3.70 / 6.81 | 4,417 | 3.70 / 6.63 |
| LRANGE_600 | 3,172 | 3.58 / 8.22 | 3,737 | 4.37 / 8.19 | 3,805 | 4.30 / 8.05 |

Unpipelined, the localhost round-trip dominates and single-command latency decides the table: single-key strings, counters, lists and sets run at 73–96% of Redis; HSET, SPOP and ZADD trail at 46–58%; multi-element `LRANGE` reads land at 63–85%. Single-key commands stay at or under 0.74 ms p50; the `LRANGE` range reads are the exception at 1.96–3.58 ms p50, since the reply grows with the number of elements returned.

::: info SPOP on large sets — fixed in v0.1.8
In v0.1.7, SPOP selected random members by iterating and cloning the entire set — O(n) per pop — which collapsed to **823 rps** against the ~100k-member set this suite builds. v0.1.8 backs sets with an index-addressable structure (`IndexSet`), making SPOP/SRANDMEMBER O(1) per member: the same large-set workload now runs at **~22,000 rps**, in line with the other set commands.
:::

## What's still on the list

- **HSET at P1** is the biggest remaining outlier (46% of Redis despite *beating* Redis pipelined) — single-command hash-write latency deserves its own investigation.
- **RESP parsing allocates a `String` per argument.** Moving commands to byte-slice arguments is the deepest remaining refactor and the main lever left for unpipelined latency.
- **LRANGE** builds the full reply `Value` before serializing; serializing straight from the store would cut the remaining gap on large range reads.

## Reproducing

```bash
# Build the server
cargo build --release -p server-native

# Terminal 1 — recached, persistence off
RECACHED_BIND=127.0.0.1 RECACHED_SAVE_INTERVAL=0 ./target/release/server-native

# Terminal 2 — run the suite (needs redis-benchmark on PATH)
scripts/benchmark.sh

# Then stop recached and repeat against Redis / Valkey:
redis-server --port 6390 --bind 127.0.0.1 --save '' --appendonly no
PORT=6390 scripts/benchmark.sh
```

Benchmark results from other hardware — especially many-core servers, where the multi-threaded runtime has the most to gain — are very welcome. Open an issue or PR with your `--csv` output and machine details.
