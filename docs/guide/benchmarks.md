# Benchmarks

Recached does not publish a current Redis or Valkey performance table. The older comparisons used different Recached releases and outdated server images; keeping their numbers on a current product page would make them look more conclusive than they are.

The supported claim is narrower: Recached schedules command execution across worker threads. Measure throughput, latency, and memory again for the exact commit, images, host, and workload you plan to deploy.

## Historical thread-scaling evidence

A previous Linux container run changed only `RECACHED_WORKER_THREADS`, using one Recached binary, one workload, and a fixed four-core server CPU set. Aggregate pipelined throughput increased as workers were added:

| Worker threads | 1 | 2 | 4 |
|---|---:|---:|---:|
| Aggregate requests/s | 481,280 | 854,111 | 1,044,313 |
| Change from one thread | baseline | +77% | +117% |

This is historical evidence that the command path used multiple workers in that build. It is not current-release throughput and does not establish that Recached is faster than Redis or Valkey.

## Run the current suite

### Reproducible Linux comparison

Build the current Recached image, then run the Docker harness:

```bash
docker build -t recached-bench:local .
scripts/bench-docker.sh
```

The default run:

- pins each server to `SERVER_CPUS` and the load generator to a disjoint `BENCH_CPUS` set;
- compares pinned Recached, Redis, and Valkey images with persistence disabled;
- records pipelined and unpipelined `redis-benchmark` CSV files;
- reruns the same Recached image with several worker counts; and
- measures process-RSS growth per live key for string, small-hash, and small-set workloads.

Results go to `bench-results/` by default. Keep `conditions.txt` beside the CSV files whenever results are shared.

Use quick mode only as a harness smoke test:

```bash
scripts/bench-docker.sh --quick
```

Useful overrides include:

```bash
SERVER_CPUS=0-7 BENCH_CPUS=8-15 N=500000 MEMORY_KEYS=250000 MEMORY_DATA=256 scripts/bench-docker.sh
```

Keep the CPU sets disjoint. Do not compare unpipelined Docker Desktop results with native-loopback results: virtualization network overhead can dominate one-command-per-round-trip measurements.

### Thread scaling without Docker

Use the same release binary while changing only the worker count:

```bash
cargo build --release --package recached
THREADS="1 2 4 8" scripts/bench-scaling.sh
```

On Linux, isolate the server and load generator when the host has enough cores:

```bash
PIN=1 SERVER_CPUS=0-3 BENCH_CPUS=4-7 scripts/bench-scaling.sh
```

The script verifies the worker count reported by the server. Compare columns within one run; do not combine absolute numbers from different machines.

### A server already running

`scripts/benchmark.sh` measures whichever RESP server is listening at the selected host and port:

```bash
RECACHED_BIND=127.0.0.1 RECACHED_SAVE_INTERVAL=0 ./target/release/recached-server
scripts/benchmark.sh
```

Repeat with identical settings for each server. Record binary versions, configuration, CPU placement, persistence mode, request count, concurrency, value size, key distribution, and pipeline depth.

## Memory results

`memory-per-key.csv` reports the change in container process RSS divided by the actual `DBSIZE`, using a fresh server process for each data shape. It complements Recached's `used_memory` and `recached_memory_bytes`, which are logical key/value counters rather than process RSS.

RSS deltas include allocator behavior and internal indexes, but short runs also contain measurement noise and lazy initialization. Use enough keys to make the delta large relative to the baseline, repeat the run, and report medians. Do not turn one result into a universal “times more memory” claim.

## Interpreting results

- Compare medians across repeated runs, not one best result.
- Separate pipelined throughput from single-request latency.
- Treat a benchmark as evidence only for the tested commands and data shapes.
- Run with production persistence, replication, TLS, and value sizes before capacity planning.
- Use process RSS for host sizing; use Recached's logical memory counter to understand its configured eviction threshold.

Recached's product distinction is the shared server-and-browser engine. Server benchmarks do not measure the browser path, where reads come from local WebAssembly memory and avoid a server round trip.
