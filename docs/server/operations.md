# Operations

Running Recached in production: what it exports, what to alert on, and what it does **not** tell you yet.

## Metrics endpoint

Recached serves Prometheus metrics on a separate port from the cache itself, so you can expose it to
your monitoring network without exposing the data plane.

```bash
RECACHED_METRICS_PORT=9090 recached-server
curl http://127.0.0.1:9090/metrics
```

The port is set by [`RECACHED_METRICS_PORT`](/server/configuration#environment-variable-reference)
and binds to the same host as `RECACHED_BIND`.

::: warning The metrics port has no authentication
It inherits `RECACHED_BIND` but not `RECACHED_PASSWORD`. Anything that can reach the port can read
your metrics — including key hit/miss volume and per-command traffic. Bind it to a private interface
or firewall it.
:::

## What is exported

Six series. That is the complete list — the exporter is deliberately small, and the gaps below are
real.

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `recached_commands_total` | counter | `command` | Commands executed, by command name. |
| `recached_command_errors_total` | counter | `command` | Commands that returned an error, by command name. |
| `recached_connections_total` | counter | `type` = `tcp` \| `ws` | Connections accepted since start, split by transport. |
| `recached_connections_active` | gauge | — | Connections currently open (TCP and WebSocket combined). |
| `recached_keyspace_hits_total` | counter | — | Reads that found a live key. |
| `recached_keyspace_misses_total` | counter | — | Reads that found nothing or an expired key. |

### Capacity and sync

Sampled every 5 seconds, because these are levels rather than events.

| Metric | Type | Meaning |
|---|---|---|
| `recached_memory_bytes` | gauge | Approximate heap used by stored data. Compare against `RECACHED_MAX_MEMORY`. |
| `recached_keys` | gauge | Live keys, excluding expired entries awaiting sweep. Compare against `RECACHED_MAX_KEYS`. |
| `recached_evictions_total` | counter | Keys evicted since start. A rising rate means the cache is working at its cap. |
| `recached_replicas_connected` | gauge | Replicas currently attached to this primary. |
| `recached_live_queries` | gauge | Registered `QSUB` patterns across all connections. |
| `recached_watched_keys` | gauge | Keys under `WATCH`. |
| `recached_dedup_clients_tracked` | gauge | Clients with exactly-once bookkeeping in memory. |
| `recached_replication_queue_depth` | gauge | Deepest replica send queue, in frames — work the primary has not yet put on the wire. |
| `recached_replication_lag_frames` | gauge | Frames the furthest-behind replica has been sent but has not acknowledged applying. Zero means every replica is caught up. |

### Reading the two replication gauges

They fail differently, which is why both exist:

- **Queue depth high, lag high** — the primary cannot hand frames off fast enough. The replica's
  channel is backing up, usually a slow or saturated network link. A replica whose queue fills is
  disconnected outright so it resyncs from a snapshot rather than falling further behind.
- **Queue depth zero, lag high** — everything was written to the socket and the replica is not
  acknowledging it. The frames are in flight, or the replica is applying them slowly, or it is
  wedged. This is the case queue depth alone cannot see, and it is the one worth alerting on.

Lag is measured in frames, not bytes or seconds: one frame is one replicated write command.

A replica running a build older than 0.2.2 never acknowledges, so its lag climbs without bound while
replication works normally. Upgrade both ends together.

### What is still not exported

- **Client outbox depth.** That state lives in the browser — read it there with
  `cache.pendingWrites()`.

## Useful queries

```promql
# Command throughput by command
rate(recached_commands_total[1m])

# Error ratio — the single most useful health signal
sum(rate(recached_command_errors_total[5m]))
  / sum(rate(recached_commands_total[5m]))

# Cache hit ratio
sum(rate(recached_keyspace_hits_total[5m]))
  / (sum(rate(recached_keyspace_hits_total[5m])) + sum(rate(recached_keyspace_misses_total[5m])))

# Connection headroom against RECACHED_MAX_CONNECTIONS (default 1024)
recached_connections_active

# WebSocket sync clients specifically
rate(recached_connections_total{type="ws"}[5m])
```

## Suggested alerts

Thresholds are starting points — tune to your traffic.

| Alert | Condition | Why it matters |
|---|---|---|
| Error-rate spike | error ratio > 1% for 5m | Usually a client sending unsupported commands or malformed args after a deploy. |
| Connection saturation | `recached_connections_active` > 80% of `RECACHED_MAX_CONNECTIONS` | New connections are rejected once the semaphore is exhausted — this fails hard, not gracefully. |
| Hit ratio collapse | hit ratio drops sharply vs baseline | Keys expiring faster than expected, an eviction storm, or a cold restart. |
| Traffic flatline | `rate(recached_commands_total[5m]) == 0` while clients are up | The process is alive enough to scrape but not serving. |
| Memory pressure | `recached_memory_bytes` > 80% of `RECACHED_MAX_MEMORY` | Eviction is about to start, or already has. |
| Eviction churn | `rate(recached_evictions_total[5m])` climbing | The working set no longer fits; results will start missing. |
| Replica lost | `recached_replicas_connected` drops | Failover risk — the standby is no longer following. |
| Replica falling behind | `recached_replication_lag_frames` > 1000 for 5m | The standby is not keeping up; a failover now would lose those writes. |

## Health checking

There is no dedicated HTTP health endpoint. Use the protocol itself:

```bash
# Liveness — is the cache answering?
redis-cli -p 6379 ping        # → PONG

# With auth enabled
redis-cli -p 6379 -a "$RECACHED_PASSWORD" ping
```

For container orchestration:

```yaml
livenessProbe:
  exec:
    command: ["redis-cli", "-p", "6379", "ping"]
  initialDelaySeconds: 5
  periodSeconds: 10
```

The `/metrics` endpoint returning 200 proves the metrics listener is up, **not** that the cache is
healthy — they are separate listeners. Probe the cache port.

For a human-readable snapshot at a terminal — uptime, connected clients, keyspace size, replication
role — use [`INFO`](/server/commands#info):

```bash
redis-cli -p 6379 INFO              # all default sections
redis-cli -p 6379 INFO replication  # just the topology
```

`INFO` and Prometheus serve different jobs and neither replaces the other: `INFO` is a point-in-time
snapshot for an operator or a client's ready-check, while `/metrics` carries the per-command
counters, error counts, and history that dashboards and alerts need. Alert on the metrics, not on
scraped `INFO` output.

## Capacity limits

Hard limits compiled into the server. Exceeding them produces errors rather than degradation, so it
is worth knowing where the walls are:

| Limit | Default | Configurable |
|---|---|---|
| Max connections | 1024 | `RECACHED_MAX_CONNECTIONS` |
| Consecutive auth failures before disconnect | 5 | No |
| Read buffer per TCP connection | 64 MB | No |
| Queued commands per `MULTI` | 10,000 | `RECACHED_MAX_MULTI_QUEUE` |
| `WATCH`ed keys per connection | 1,024 | `RECACHED_MAX_WATCHES_PER_CONN` |
| Live queries (`QSUB`) per connection | 64 | `RECACHED_MAX_LIVE_QUERIES` |
| Keys returned in a live query's initial state | 10,000 | `RECACHED_MAX_QSUB_INITIAL_KEYS` |
| Keys sampled per eviction pass | 10 | `RECACHED_EVICTION_SAMPLE` |
| Replication frame | 512 MB | No |
| Glob pattern length (`KEYS`, `SCAN MATCH`, `PSUBSCRIBE`, sync scopes) | 1,024 bytes | No |
| Elements reserved up front for an aggregate | 1,024 | No |
| Client outbox (browser, offline writes) | 10,000 writes | via `sync-client` |

The keyspace cap (`RECACHED_MAX_KEYS`) and memory cap (`RECACHED_MAX_MEMORY`) are configured rather
than compiled — see [Configuration](/server/configuration#environment-variable-reference).

## Backup and restore

Snapshots are MessagePack files at `RECACHED_SAVE_PATH`, written atomically (temp file plus rename),
so copying the file while the server runs is safe.

```bash
# Take a snapshot on demand, then confirm it completed
redis-cli -p 6379 BGSAVE
redis-cli -p 6379 LASTSAVE     # timestamp advances when the save lands

# Back up
cp /var/lib/recached/dump.msgpack /backups/dump-$(date +%F).msgpack
```

A sidecar file sits next to the snapshot with a `.dedup` extension, holding exactly-once high-water
marks. Back it up with the snapshot: without it a restarted server can re-apply a write a client
replays. Losing it is not fatal — the server starts normally and rebuilds the marks.

To restore, stop the server, put the snapshot (and its `.dedup` sidecar) at `RECACHED_SAVE_PATH`, and
start it — both load at boot. There is **no import path from a Redis RDB file**; the formats are unrelated.

If AOF is enabled, the AOF replays on top of the snapshot. Losing the AOF while keeping the snapshot
costs you every write since the last save.

## Upgrades

Recached is pre-1.0 and the wire protocol is not frozen — see the
[protocol spec](/server/protocol). Read the [changelog](https://github.com/recached-dev/recached/blob/main/CHANGELOG.md)
before upgrading a minor version, and upgrade server and browser SDK together: the sync protocol is
versioned between them, and mixed versions are not a supported configuration.

Snapshot format is backward compatible — a newer server reads an older snapshot. The reverse is not
guaranteed, so keep a copy of the pre-upgrade snapshot if you may need to roll back.
