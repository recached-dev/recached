# Troubleshooting

Symptoms, causes, and fixes — ordered roughly by how often each one bites.

## Server

### The server accepts no connections at all

Everything times out; the process is running and logs look normal apart from a warning at startup.

**Most likely: a malformed `RECACHED_ALLOW_IPS`.** The allowlist accepts **exact IP addresses only**.
CIDR ranges, hostnames, and typos are logged as warnings and silently dropped. If every entry is
invalid the list ends up empty — and an empty allowlist rejects **every** connection.

```
WARN RECACHED_ALLOW_IPS: ignoring invalid entry '10.0.0.0/8'
```

Check the startup logs for that warning and for which mode you are in:

```
INFO  IP allowlist ENABLED: [10.0.1.5]     ← list parsed, only these IPs
WARN  IP allowlist DISABLED. Accepting all connections.
```

Fix by expanding CIDR to individual addresses, or unset the variable and enforce the boundary at the
firewall. See [Security → IP allowlisting](/server/security#ip-allowlisting).

### `NOAUTH Authentication required.`

`RECACHED_PASSWORD` is set and the client did not authenticate.

```bash
redis-cli -p 6379 -a "$RECACHED_PASSWORD" ping
# ioredis
new Redis({ port: 6379, password: process.env.RECACHED_PASSWORD })
```

### Connection closes after a few failed attempts

Five consecutive `AUTH` failures close the connection. Reconnecting resets the counter — this is
brute-force friction, not a ban. Confirm the client is sending the password you think it is.

### `READONLY You can't write against a read only replica.`

You are writing to a replica. Writes go to the primary; replicas serve reads.

If this is a failover scenario and the replica *should* now be primary, promote it:

```bash
redis-cli -p 6379 REPLICAOF NO ONE
```

Note that `REPLICAOF host port` is **not** supported at runtime — re-pointing a server at a different
primary requires a restart with a new `RECACHED_REPLICAOF`.

### New connections are refused under load

`RECACHED_MAX_CONNECTIONS` (default **1024**) is exhausted. Connections past the limit are rejected
outright rather than queued. Watch `recached_connections_active` and raise the limit, or find the
client that is leaking connections.

### TLS does not seem to be active

If either `RECACHED_TLS_CERT` or `RECACHED_TLS_KEY` is missing, the server **falls back to plaintext
without erroring**. Verify rather than assume: a `rediss://` client should connect and a plaintext
client should fail. See [Security → Transport encryption](/server/security#transport-encryption).

### Memory keeps growing

There is no built-in memory metric — monitor process RSS. Set `RECACHED_MAX_MEMORY` and
`RECACHED_EVICTION` so the cache bounds itself, and `RECACHED_MAX_KEYS` if key count rather than
value size is the driver. `DBSIZE` reports the current key count on demand.

### `ERR key too large`

A key exceeded the maximum key length. Keys are identifiers, not payloads — put the data in the
value.

## Browser sync

### Reads work but nothing ever updates

Reads come from local WASM memory, so they succeed even with no server connection at all. A cache
that reads fine but never changes is usually **not connected**.

1. Confirm you passed `connect: { url }` to `createCache()` — without it you get a purely local
   cache, which is a supported mode and looks identical until data goes stale.
2. Check the browser console for WebSocket errors.
3. Confirm the URL scheme matches the server: `ws://` for plaintext, `wss://` when TLS is enabled.
   A page served over HTTPS cannot open a `ws://` socket — browsers block mixed content.

### `NOSCOPE key '<key>' is outside this connection's sync scopes`

The connection is scoped and the key falls outside the granted patterns. Scoping is prefix-based, so
a grant of `cart:*` admits `cart:42:item:1` but a grant of `cart:42:*` does not admit `cart:99:*`.

Check what the token actually granted before widening it — the scopes are a security boundary, and
the instinct to broaden them until the error stops is how that boundary gets lost. See
[Sync Scopes](/server/sync-scopes).

### Keyspace-wide commands fail on a browser connection

`KEYS`, `SCAN`, `DBSIZE`, `FLUSHDB`, `SAVE`, `BGSAVE`, and `REPLICAOF` are refused entirely on scoped
connections — by design, since they would leak or destroy data outside the connection's scope. Use
`liveQuery(pattern)` / `getMatching(pattern)` to work with a set of keys instead.

### Offline writes went missing

The client outbox holds **10,000 pending writes**. Past that, each new write **evicts the oldest one**
— silently, with no error surfaced to your code.

If a client can be offline long enough to exceed 10,000 writes, do not rely on the outbox as the
system of record for those mutations. Batch them, or persist them yourself and reconcile on
reconnect.

### A write applied twice

It should not — every store write carries a `DEDUP` envelope and the server skips ids at or below the
client's high-water mark, replying `+DUP`.

The documented residual: dedup marks live in **server memory** and are swept after 24 h idle. A
server restart inside the acknowledgment window can admit one duplicate. If your workload cannot
tolerate that, make the operation idempotent at the application level.

### Concurrent writes clobber each other

`set` is last-writer-wins **by server arrival order**, which is not the same as wall-clock order.
For values that must merge rather than overwrite, use the operation that matches the shape:

- Counters → `incr` / `decr` (deltas merge additively)
- Documents → `jmerge` (RFC 7386 deep merge)
- Collections → the collection commands, which replay as operations

See [Offline & Reconnection](/browser/offline).

### Live query returned fewer keys than expected

A live query's initial state is capped at **10,000 keys**. Beyond that the snapshot is truncated.
Narrow the pattern.

Also note two documented limits of live queries: `FLUSHDB` does not emit per-key diffs, and
collection values arrive as type markers rather than full values — subscribe, then re-read with
`HGETALL` / `LRANGE` on change.

### Too many live queries

64 per connection. Consolidate with broader patterns rather than opening one subscription per key.

### Cross-tab updates are not appearing

BroadcastChannel is same-origin. Tabs must share protocol, host, and port. It also does not cross
browser profiles, containers, or incognito boundaries.

## Data & persistence

### Data disappeared after restart

Confirm persistence is actually on. `RECACHED_SAVE_INTERVAL=0` disables autosave — which is what the
benchmark configuration uses, so it is easy to inherit from a copied command line.

```bash
redis-cli -p 6379 LASTSAVE   # timestamp of the last successful snapshot
```

If it returns the server start time, no snapshot has ever completed.

### Can I load a Redis RDB file?

No. Snapshots are MessagePack and unrelated to RDB. Migrating from Redis means starting with a cold
cache and letting it fill — see [Migrating from Redis](/guide/use-cases#migrating-from-redis).

## Getting help

Include the server version, the startup log lines (they state which subsystems are enabled), the
exact error string, and whether the client is RESP or the browser SDK:
[open an issue](https://github.com/thinkgrid-labs/recached/issues).
