# Changelog

All notable changes to Recached are documented here.

---

## [0.1.2] — 2026-05-08

### Added

**Sharded `DashMap` core** (`core-engine`)
- Replaced `Arc<RwLock<HashMap>>` with `Arc<DashMap>` throughout the store. Shard-level locking eliminates the write bottleneck that serialized all mutations on a single lock; concurrent throughput now scales with CPU core count.

**Native TLS** (`server-native`)
- Both the RESP TCP listener (port 6379) and the WebSocket listener (port 6380) can be wrapped in TLS. Set `RECACHED_TLS_CERT` and `RECACHED_TLS_KEY` to PEM paths to enable; either missing falls back to plain TCP with a warning. Powered by `tokio-rustls` — no sidecar required.

**Prometheus metrics** (`server-native`)
- Built-in HTTP metrics endpoint at `http://0.0.0.0:9091/metrics` (port overridable via `RECACHED_METRICS_PORT`). Exposes:
  - `recached_commands_total` — per-command counters
  - `recached_command_errors_total` — per-command error counters
  - `recached_keyspace_hits_total` / `recached_keyspace_misses_total`
  - `recached_connections_total` / `recached_connections_active`

**Pluggable eviction** (`core-engine`, `server-native`)
- When `RECACHED_MAX_KEYS` is set, Recached can now evict keys instead of immediately returning an error. Configured via `RECACHED_EVICTION`:
  - `allkeys-lru` / `lru` — evict the least recently written key from a sample of 10
  - `allkeys-random` / `random` — evict a random key
  - `volatile-lru` — evict the least recently written key that has a TTL set
  - `volatile-ttl` / `ttl` — evict the key with the nearest expiry from a sample of 10
  - Default (`noeviction`) — return an error when the cap is reached (previous behaviour)
- Sampling uses reservoir selection over 10 candidates so eviction overhead is O(1) and independent of keyspace size.

**Observable keys** (`core-engine`, `server-native`)
- New WebSocket-only commands: `WATCH key [key ...]` and `UNWATCH [key ...]`.
- When a watched key is mutated by any client — TCP backend or another WebSocket connection — the server immediately pushes a `keychange` notification to all registered watchers without polling.
- Push format is a RESP array: `["keychange", "<key>", <value>]`. String keys include the current value; complex types (Hash/List/Set/Sorted Set) include a type-name hint so the client can call the appropriate fetch command. Deleted or expired keys push nil.
- `UNWATCH` with no arguments clears all watches for the current connection. Watches are automatically cleaned up on disconnect.

### Changed

- `KeyValueStore::with_max_keys()` preserved for compatibility; new `KeyValueStore::with_config(max_keys, eviction_policy)` is the canonical constructor for server startup.
- The `Entry` struct gained a `written_at_ms` field (set at write time) to support LRU ordering without a separate access-time bookkeeping structure.
- `store.execute()` returns `ERR WATCH/UNWATCH only supported over WebSocket` if those commands reach the TCP path — they are intercepted and handled in the WebSocket handler before hitting the store.

---

## [0.1.1] — initial release

- RESP protocol parser and serializer (depth-limited, fragmentation-safe)
- TCP server on port 6379 compatible with any Redis client
- WebSocket server on port 6380 for WASM browser clients
- Mutation broadcast: any write on TCP or WebSocket is fanned out to all connected WebSocket clients
- Sender-ID filter: browser clients do not double-apply their own mutations
- `RECACHED_PASSWORD` with brute-force lockout (5 failures)
- `RECACHED_ALLOW_IPS` with validated IP parsing
- `RECACHED_MAX_KEYS` hard cap
- Connection semaphore (max 1024 concurrent)
- Background active eviction (1 s sweep) + lazy eviction on every read
- Structured `tracing` logs (`RUST_LOG`)
- Full command set: strings, expiry, keys, hash, list, set, sorted set, transactions (`MULTI`/`EXEC`/`DISCARD`), pub/sub with glob pattern matching
