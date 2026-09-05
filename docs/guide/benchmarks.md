# Benchmarks

How the Recached server compares to Redis 7.2.5 and Valkey 9.1.0 under `redis-benchmark`. Current release: **v0.2.4**.

::: warning The three-way table is v0.1.8 evidence
The Recached / Redis / Valkey tables below were measured in July 2026 on v0.1.8 and have **not** been re-run since. Two releases have landed on top of them: v0.2.0 added the exactly-once `DEDUP` envelope on every store write and extracted the sync client, and v0.2.4 reworked the store's write path and rebuilt sorted sets.

For v0.2.4 those command paths were A/B tested against the previous release and all moved within run-to-run noise — see [What changed in v0.2.4](#what-changed-in-v0-2-4) — but that was a Recached-vs-Recached comparison on a loaded machine, not a fresh three-way run. Treat the absolute figures as v0.1.8 evidence. Redis 7.2.5 was also current when this ran; newer Redis releases may perform differently.
:::

::: tip TL;DR
**Recached uses every core, and that is measurable.** Holding the machine, binary and workload fixed and varying only the worker-thread count, aggregate pipelined throughput rises **+117% from 1 thread to 4** — see [thread scaling](#thread-scaling).

**That is not a claim to be faster than Redis or Valkey.** Against stock configurations Recached leads on most pipelined commands, but stock means `io-threads 1` and neither project is meant to run that way. [Tuned](#with-io-threads-enabled), a Valkey with I/O threading is comfortably ahead of Recached, and Redis is a coin flip. Recached threads command *execution* rather than only I/O, which wins on compute-heavy commands like `ZADD` and little else.

Unpipelined — one command per round-trip, the traffic shape of typical application cache calls — Recached runs at 46–96% of Redis with **sub-millisecond p50 latency on every single-key command** (multi-element `LRANGE` reads are the exception, at 1.96–3.58 ms p50).
:::

Recached's design goal is not to beat Redis at raw server throughput — it is to remove the network round-trip entirely for browser reads, which no server-side cache can do. These numbers cover the server half (`server-native`) so you know what to expect when you point existing Redis clients at it.

## Environment

| | |
|---|---|
| Hardware | Intel Core i5-8259U (4 cores / 8 threads, 2.3 GHz), 8 GB RAM |
| OS | macOS (Darwin 24.6.0) |
| Recached | v0.1.8, `cargo build --release` (thin LTO, jemalloc) — see the v0.2.4 notes below |
| Redis | 7.2.5 (Homebrew) |
| Valkey | 9.1.0 (Homebrew) |
| Load generator | `redis-benchmark` from Redis 7.2.5 |

Methodology:

- Servers ran **one at a time** on localhost, with the load generator on the same machine.
- Persistence disabled everywhere: `RECACHED_SAVE_INTERVAL=0` for Recached; `--save '' --appendonly no` for Redis and Valkey.
- Each server got a 10k-request warm-up, then `FLUSHDB`, then the measured run: **100,000 requests, 50 parallel connections, 64-byte values, randomized keys** (`-n 100000 -c 50 -d 64 -r 100000`), tests back-to-back in suite order.
- Reproduce with [`scripts/benchmark.sh`](https://github.com/recached-dev/recached/blob/main/scripts/benchmark.sh).

One caveat: this is a 4-core laptop and the benchmark tool competes with the servers for cores — absolute numbers on server hardware will be higher for all three systems. Recached's multi-threaded runtime has the most headroom to gain from more cores, since Redis executes commands on a single thread.

Valkey deserves a footnote it did not get when these numbers were taken. Valkey 8 added threaded I/O — reading, parsing and writing happen on several threads even though command execution still does not. A [later Linux cross-check](#a-linux-cross-check) against Valkey 9.1.2 shows it much closer to Recached than the 9.1.0 figures below suggest, and ahead on several commands. Treat "Redis and Valkey are single-threaded" as true of command execution only, and as increasingly misleading about Valkey's throughput.

## Thread scaling

Recached executes commands on every core; Redis executes them on one. The tables further down compare Recached against Redis and Valkey, but a cross-server comparison can never prove *why* one is faster — a different binary, allocator and data structure are all changing at once.

This section changes one thing. Same container, same CPU set, same workload, same binary. Only `RECACHED_WORKER_THREADS` moves.

Linux (containers), server pinned to 4 cores, load generator pinned to 4 others, `-P 16`, 100k requests. Requests/sec:

| Command | 1 thread | 2 threads | 4 threads | 1→4 |
|---|---:|---:|---:|---:|
| SET | 49,140 | 98,039 | 102,881 | **+109%** |
| GET | 110,375 | 226,244 | 187,970 | +70% |
| INCR | 44,944 | 122,850 | 152,905 | **+240%** |
| LPUSH | 53,850 | 122,699 | 145,985 | **+171%** |
| SADD | 69,784 | 89,606 | 170,940 | **+145%** |
| HSET | 77,942 | 84,175 | 132,802 | +70% |
| ZADD | 75,245 | 110,497 | 150,830 | **+100%** |
| **Aggregate** | **481,280** | **854,111** | **1,044,313** | **+117%** |

Every command gains, and the aggregate more than doubles from one thread to four. A single-threaded server cannot produce this curve at all — its throughput is flat in the number of cores, which is the whole reason Redis scales by running more processes.

Scaling is sub-linear, as expected: 4× the threads buys 2.2× the throughput. Shard contention, the load generator's own ceiling, and the fact that these are 4 physical cores of a hyperthreaded laptop all take a cut. `GET` peaks at 2 threads here — it is the cheapest command in the set, so per-command dispatch cost stops mattering soonest.

Reproduce with `scripts/bench-scaling.sh` (no Docker needed) or `scripts/bench-docker.sh`. Both abort if the server reports a different worker count than the one requested, so a run cannot silently measure the wrong thing.

::: warning Absolute numbers here are lower than the tables below
These were measured inside Docker Desktop's VM on the same 2018 laptop, so they carry both virtualization overhead and only 4 cores for the server. Compare the *columns to each other*, not to the v0.1.8 figures further down. The ratio is the result; the absolute rps is not.
:::

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

This is where multi-threading pays: Recached spreads 50 connections across all cores, while Redis and Valkey execute commands on one. p50 latency stays around 0.7–1.0 ms and p99 under 5.1 ms across the suite. The [thread-scaling table](#thread-scaling) isolates the effect directly.

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

## A Linux cross-check

The tables above were measured natively on macOS in July 2026. In September 2026 the same suite was re-run on Linux through `scripts/bench-docker.sh`, with the server and load generator on disjoint CPU sets. Two things came out of it worth recording.

**Pipelined, Recached still leads Redis — but no longer leads Valkey.** Requests/sec, server pinned to 4 cores:

| Command | Recached | Redis 7.2.5 | Valkey 9.1.2 |
|---|---:|---:|---:|
| SET | **143,678** | 78,003 | 142,248 |
| GET | 169,492 | 84,531 | **219,780** |
| INCR | **158,479** | 104,384 | 149,701 |
| LPUSH | 163,934 | 125,628 | **198,020** |
| SADD | 112,740 | 103,520 | **212,766** |
| HSET | 117,647 | 119,474 | **123,457** |
| ZADD | **122,549** | 49,092 | 79,872 |

Recached beats Redis on 6 of 7 (HSET is a 2% loss, inside noise). Against Valkey 9.1.2 it wins 3 of 7 — a real change from the 7-of-7 sweep against 9.1.0 above. But note that **all three servers here are single-threaded on the command path**: Redis and Valkey were left at their default `io-threads 1`. See [With `io-threads` enabled](#with-io-threads-enabled) for what happens when they are not, which is the comparison a tuned deployment should care about.

::: danger Do not run unpipelined benchmarks on Docker Desktop
Docker Desktop for Mac and Windows emulates the network in userspace (gVisor). Measured with `redis-cli --latency` inside the VM, a round-trip costs **~0.82 ms**, against roughly 0.05 ms on a native loopback. Pipelining spreads that over 16 commands and survives; an unpipelined test pays it once per command, capping throughput at about 1,250 rps *per connection* no matter which server is running.

That is exactly what happened: in the same run Redis measured 9,280 unpipelined SET/sec, roughly a sixth of its native figure. The harness now prints a warning on non-Linux hosts. The unpipelined table above stands as the native measurement; publishable unpipelined numbers need a native Linux host.
:::

## With `io-threads` enabled

Every table above compares against **stock** Redis and Valkey. Both ship `io-threads 1` — single-threaded — and both have supported I/O threading for years (Redis since 6.0, reworked in Valkey 8). Benchmarking only the default is comparing against a configuration no tuned deployment runs, so here is the other half.

Back-to-back on Linux, all three tuned, server pinned to 4 cores, `-P 16`. Requests/sec:

| Command | Recached (4 workers) | Redis `io-threads 4` | Valkey `io-threads 4` |
|---|---:|---:|---:|
| SET | **104,058** | 75,358 | 79,365 |
| GET | 166,113 | 202,429 | **228,833** |
| INCR | 94,877 | 97,561 | **141,643** |
| LPUSH | 72,569 | 125,156 | **373,134** |
| SADD | 152,207 | 86,580 | **259,740** |
| HSET | **110,375** | 101,833 | 106,383 |
| ZADD | **153,846** | 67,568 | 104,167 |
| **Aggregate** | 854,045 | 756,485 | **1,293,265** |

**A tuned Valkey is faster than Recached at server-side throughput, and it is not close.** Recached against a tuned Redis is a coin flip — two runs of this table put Redis at 1,190,303 and 756,485, a 36% swing, which is the laptop this ran on rather than anything about Redis. Do not draw a conclusion from that row.

What does survive is narrower than "multi-threaded". Redis and Valkey thread their *I/O* — sockets, parsing, reply writing — while still executing commands on one thread. Recached threads *execution* over a sharded keyspace. That only pays when execution cost dominates the command, which is why `ZADD` is Recached's one consistent win across every run, and why it loses on `GET` and `LPUSH`, where syscall and parsing cost dominate and I/O threading captures the win instead.

So: use the [thread-scaling table](#thread-scaling) as evidence that Recached uses the cores you give it. Do not use it as evidence that Recached out-throughputs a tuned Valkey — it does not. The architectural claim that no amount of tuning answers is the [browser half](/guide/introduction), where the round-trip disappears entirely.

## What changed in v0.2.4

v0.2.4 rebuilt sorted sets around a score-ordered index. Before it, every range
command — `ZRANGE`, `ZREVRANGE`, `ZRANGEBYSCORE`, `ZCOUNT`, `ZRANK` — collected
the whole set into a vector and sorted it, O(n log n) per query, while holding
the shard guard for that key. `ZRANGE board 0 9` on a large leaderboard sorted
every member to return ten, and blocked every other key in the same shard while
it did.

The index is built on first use and maintained only while something is reading
it, so a set nobody runs a range query against pays nothing for it.

| Workload (~45k-member sorted set) | v0.2.3 | v0.2.4 | |
|---|---:|---:|---:|
| Repeated `ZRANGE key 0 9`, no writes | 244 rps | 133,333 rps | **546× faster** |
| Alternating `ZADD` + `ZRANGE`, 3,000 pairs | 65.04 s | 0.11 s | **597× faster** |
| `ZADD` throughput, `-P 16`, growing set | 387k rps | 330k rps | **~15% slower** |

The `ZADD` cost is the trade: a write that keeps an ordering current pays for it.
Maintaining the index unconditionally cost ~48% of `ZADD` throughput, which is
why it is built lazily and abandoned again when writes run far ahead of reads —
that recovers most of it. A write-only sorted set never builds an ordering at
all and writes at pre-v0.2.4 speed.

::: info How these were measured
Recached v0.2.3 vs v0.2.4 binaries, same machine, same session, alternating
round-robin with medians over 4–7 rounds. **The machine was under other load, so
read these as before/after ratios, not as absolute throughput** — the absolute
numbers are not comparable to the v0.1.8 tables above, which ran on an idle
machine. Every other command in the suite (`SET`/`GET`/`INCR`/`LPUSH`/`SADD`/
`HSET`, pipelined and not) moved within its own run-to-run spread.
:::

The other v0.2.4 changes were correctness- or memory-driven rather than
throughput work, and none of them moved the command table:

- **`maxmemory` is now enforced on the write path**, not only by the once-a-second
  background sweep, so a burst can no longer run past the cap between ticks. The
  check is two atomics per write; a full keyspace measurement is paid for only
  when that cheap estimate says the cap is near.
- **Partial frames are no longer re-parsed from scratch on every TCP segment.**
  A large multi-bulk arriving over hundreds of segments used to rebuild — and
  reallocate — every element received so far, once per segment, and throw it
  away. Completeness is now decided by a non-allocating measure, so streaming a
  420 KB frame allocates over 10× fewer bytes.

## What's still on the list

- **A fresh three-way run on an idle machine.** The Recached / Redis / Valkey
  tables are still v0.1.8 measurements. This is the top of the list.
- **HSET at P1** is the biggest remaining outlier (46% of Redis despite *beating* Redis pipelined) — single-command hash-write latency deserves its own investigation.
- **RESP parsing allocates a `Vec` per argument.** v0.2.4 removed the allocation
  from the *incomplete*-frame path, which is the one a streaming read hits most,
  but a frame that does arrive still copies each argument out. Moving commands to
  borrowed byte-slice arguments is the deepest remaining refactor and the main
  lever left for unpipelined latency.
- **`ZADD` gives up ~15%** against a sorted set that is actively being read, to
  keep the score index current. Sharing the member allocation between the map and
  the index, rather than holding an `Arc` per member, is where the rest of that
  would come from.
- **LRANGE** builds the full reply `Value` before serializing; serializing straight from the store would cut the remaining gap on large range reads.

## Reproducing

Three scripts, in increasing order of rigour. The crate is `recached` and the binary it builds is `recached-server`.

### Thread scaling — `scripts/bench-scaling.sh`

The one that answers "is it really multi-threaded". No Docker required, so this is the script to run on a real server.

```bash
cargo build --release --package recached

# Varies RECACHED_WORKER_THREADS and nothing else.
THREADS="1 2 4 8" scripts/bench-scaling.sh

# On Linux, give the load generator its own cores — without this it competes
# with the server and the curve flattens, understating the effect.
PIN=1 SERVER_CPUS=0-3 BENCH_CPUS=4-7 scripts/bench-scaling.sh
```

The script aborts if the server reports a different worker count than the one requested, so a run can never silently measure the wrong thing.

### Three-way on Linux — `scripts/bench-docker.sh`

Runs Recached, Redis and Valkey as containers from pinned images, with the server and the load generator on disjoint CPU sets.

```bash
docker build -t recached-bench:local .
scripts/bench-docker.sh                  # or --quick for a smoke test
SERVER_CPUS=0-7 BENCH_CPUS=8-15 scripts/bench-docker.sh
```

Every run writes `conditions.txt` next to the CSVs, recording the image IDs, kernel, CPU sets and workload — a benchmark without its conditions is an anecdote.

### Against a running server — `scripts/benchmark.sh`

The original harness. Benchmarks whatever is already listening, wherever you started it.

```bash
RECACHED_BIND=127.0.0.1 RECACHED_SAVE_INTERVAL=0 ./target/release/recached-server
scripts/benchmark.sh

# Then stop it and repeat against Redis / Valkey:
redis-server --port 6390 --bind 127.0.0.1 --save '' --appendonly no
PORT=6390 scripts/benchmark.sh
```

Benchmark results from other hardware — especially many-core servers, where the multi-threaded runtime has the most to gain — are very welcome. Open an issue or PR with your `--csv` output and machine details.
