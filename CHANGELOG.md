# Changelog

All notable changes to Recached are documented here.

---

## [0.1.5] — 2026-05-10

### Added

**`recached-react` package** (new)
- `<RecachedProvider>` — React context provider that initialises the cache and makes it available to the component tree. Accepts `options` (passed to `createCache`) or a pre-built `cache` instance.
- `useRecached()` — returns the `Cache` instance from context; throws if used outside a provider.
- `useKey(key)` — returns `string | null`, re-renders on any mutation from any source (local write, WebSocket fan-out, or BroadcastChannel). Implemented with React 18 `useSyncExternalStore` — concurrent-safe, no tearing.
- `useKeyJSON<T>(key)` — like `useKey` but deserialises the value via `cache.getJSON<T>`.
- `usePubSub(channel, handler)` — subscribes to a pub/sub channel on mount, invokes `handler(msg)` on each message, and cleans up on unmount.

**`recached-vue` package** (new)
- `RecachedPlugin` — Vue 3 plugin (`app.use(RecachedPlugin, options)`) that creates the cache and provides it via `inject`/`provide`.
- `useRecached()` — injects the `Cache` instance; throws if the plugin was not installed.
- `useKey(key)` — returns a `Ref<string | null>` that updates reactively on any mutation.
- `useKeyJSON<T>(key)` — like `useKey` but deserialised via `cache.getJSON<T>`.
- `usePubSub(channel, handler)` — subscribes on call, unsubscribes via `onUnmounted`.

**RESP3 Push frame** (`core-engine`, `server-native`, `wasm-edge`)
- New `Value::Push(Vec<Value>)` variant in the RESP parser/serialiser (`>N\r\n…`). Push frames are unambiguously out-of-band — no heuristic needed to distinguish mutation fan-out from command responses.
- All server-to-WebSocket mutation fan-out and pub/sub messages now use Push frames instead of Array frames.
- `wasm-edge` `connect()` handler pattern-matches on `Value::Push` first; Array frames are ignored (they are command acknowledgements, not mutations).

**Mutation notification bus** (`wasm-edge`, SDK)
- `RecachedCache::set_mutation_callback(cb)` — WASM method that fires `cb()` after every write from any source (local call, WebSocket Push, or BroadcastChannel).
- `RecachedCache::set_message_callback(cb)` — WASM method that fires `cb(channel, message)` on each pub/sub Push frame.
- `Cache.onMutation(cb): () => void` — public SDK method; returns an unsubscribe function.
- `Cache.onMessage(channel, cb): () => void` — pub/sub listener registration; returns an unsubscribe function.

**Auto-failover** (`server-native`)
- New `RECACHED_FAILOVER_TIMEOUT` env var. When set on a replica, it starts a timer the first time the primary becomes unreachable. If the primary is still unreachable after the configured number of seconds, the replica promotes itself to primary and begins accepting writes. The timer resets on successful reconnect, so brief primary restarts do not trigger spurious promotion.

**Replication backpressure** (`server-native`)
- Replica channels switched from `mpsc::UnboundedSender` to bounded `mpsc::Sender`. Channel capacity is controlled by `RECACHED_REPL_BUFFER` (default: `4096` frames). A replica that falls this many writes behind is disconnected and must reconnect from a fresh snapshot — the primary write path is never blocked by a slow replica.

**Persistence hardening** (`core-engine`, `server-native`, `wasm-edge`)
- **Dirty counter** — `KeyValueStore` now tracks a `dirty: Arc<AtomicU64>` write counter. Every successful write command increments it; `save()` resets it to zero. Autosave skips the snapshot entirely when `dirty == 0`, so idle servers produce no disk I/O.
- **Multi-condition save policy (`RECACHED_SAVE`)** — replaces the single-interval `RECACHED_SAVE_INTERVAL` with a Redis-compatible multi-condition format: `"seconds:changes[,seconds:changes...]"`. A snapshot fires when any condition is satisfied (`elapsed >= secs && dirty >= changes`). `RECACHED_SAVE_INTERVAL` still works as a single-condition fallback for backward compatibility. Example: `RECACHED_SAVE="900:1,300:10,60:10000"`.
- **WAL compaction on load** (`wasm-edge`) — when `enable_persistence()` replays more than 1 000 WAL entries, it compacts in-place: clears IndexedDB, then writes the current in-memory state as minimal RESP commands (`SET PX` for string TTLs, `HSET`, `RPUSH`, `SADD`, `ZADD`, plus `PEXPIREAT` for collection TTLs). Next startup replays only N snapshot entries instead of the full write history.

### Changed

- Autosave loop now polls every second against save conditions rather than sleeping for the full interval. This allows multi-condition triggers (e.g. "10 000 writes in 60 s") to fire promptly.
- `ServerState::save()` calls `store.reset_dirty()` after a successful snapshot, ensuring the dirty counter accurately reflects only writes since the last save.

---

## [0.1.4] — 2026-05-09

### Added

**Snapshot persistence** (`server-native`)
- New `SAVE` command: blocks until the snapshot is written to disk and returns `+OK`.
- New `BGSAVE` command: spawns a background Tokio task to write the snapshot and immediately returns `+Background saving started`. The server continues accepting connections during the save.
- New `LASTSAVE` command: returns the Unix timestamp (seconds) of the most recent successful save as an integer.
- On startup, the server loads the snapshot from disk before accepting connections. Expired keys are silently skipped during restore.
- On clean shutdown (SIGTERM or Ctrl-C), a final snapshot is saved before the process exits.
- Periodic autosave runs every `RECACHED_SAVE_INTERVAL` seconds (default: 900 = 15 min). Set to `0` to disable autosave while keeping `SAVE`/`BGSAVE`/`LASTSAVE` available.
- Snapshot path is controlled by `RECACHED_SAVE_PATH` (default: `recached.rdb` in the working directory).
- Snapshot format: [MessagePack](https://msgpack.org/) via `rmp-serde`. Atomic write: data is written to a `.tmp` file then renamed, so a crash mid-save cannot corrupt the previous snapshot.
- All data types are preserved: strings, hashes, lists, sets, sorted sets, and TTLs.
- `Command::Save`, `Command::BgSave`, `Command::LastSave` added to `core-engine`; handled by the server before reaching `execute_and_record` since they require async filesystem I/O.
- `SnapshotEntry` and `SnapshotValue` public types added to `core-engine::store`.
- `KeyValueStore::snapshot()` and `KeyValueStore::restore()` methods added.

**AOF persistence** (`server-native`)
- New `RECACHED_AOF_PATH` env var. When set, every successful write is appended to the file as a normalized RESP command immediately after execution, in addition to periodic snapshot saves.
- New `RECACHED_AOF_SYNC` env var controlling fsync policy: `always` (after every write), `everysec` (background flush once per second, default), `no` (OS-managed).
- On startup: snapshot is loaded first, then AOF commands are replayed for the delta — recovering writes made after the last snapshot.
- After each successful snapshot save, the AOF is automatically truncated. The snapshot subsumes the log, so on the next startup only the post-snapshot delta is replayed.
- Combined with snapshots, the maximum data loss window is bounded by the AOF sync interval (≤1 second with `everysec`) rather than the snapshot interval.
- `APPEND` command added to the write broadcast path so it is captured by AOF and replication.

**Leader-follower replication** (`server-native`)
- New `RECACHED_REPLICAOF=host:port` env var. When set, the server runs as a read-only replica: it connects to the primary, loads a full snapshot, then streams all subsequent write commands in real time.
- New `RECACHED_REPL_PORT` env var (default: `6381`). The primary listens on this port for incoming replica connections.
- Initial sync protocol: the primary registers the replica's write channel first (so writes during snapshot serialization are buffered), serializes the full store to MessagePack, sends it length-prefixed over TCP, then streams subsequent writes as length-prefixed RESP strings.
- Replicas reject all write commands with `-READONLY You can't write against a read only replica.`
- Replicas reconnect automatically with exponential backoff (2 s → 4 s → … → 30 s cap) if the primary is temporarily unavailable.
- All writes on the primary flow through the unified `ServerState::on_write()` path, which handles AOF append and replica fan-out in a single call.

**Configurable connection limit** (`server-native`)
- New `RECACHED_MAX_CONNECTIONS` env var (default: `1024`). Raising this allows high-traffic deployments to accept more concurrent clients without rebuilding from source.

---

## [0.1.3] — 2026-05-09

### Added

**IndexedDB persistence** (`wasm-edge`)
- New `enable_persistence(): Promise<void>` method on `RecachedCache`. Opens (or creates) an IndexedDB database named `recached` with a `wal` object store, replays all stored commands into the in-memory store, and enables write-through persistence for future mutations.
- Every locally-initiated `set`, `set_ex`, and `del` is appended to the WAL as a RESP-encoded command with a monotonically-increasing sequence number. Writes are fire-and-forget (`spawn_local`) so the synchronous call path is unaffected.
- On page refresh, calling `enable_persistence()` again replays the WAL and restores the full cache state before any network round-trip, eliminating blank-state flicker.
- New `clear_persistence(): Promise<void>` method erases the WAL without touching the in-memory store — useful for sign-out flows.
- IndexedDB I/O is implemented as inline JavaScript (`openRecachedDb`, `idbReadAll`, `idbAppend`, `idbClear`) exposed to Rust via `wasm_bindgen(inline_js)`, avoiding the verbosity of raw `web-sys` IDB bindings.
- Added `wasm-bindgen-futures` dependency to `wasm-edge` to support `spawn_local` and `future_to_promise`.

**TypeScript SDK** (`wasm-edge`)
- New `sdk.ts` provides a fully-typed, ergonomic wrapper over the raw wasm-bindgen bindings:
  - `createCache(options?): Promise<Cache>` — single entry point; handles WASM init (lazy singleton), persistence hydration, BroadcastChannel setup, and server connection in one call.
  - `init(): Promise<void>` — eagerly pre-loads the WASM module for latency-sensitive paths.
  - `Cache.get(key)` returns `string | null` (not `string | undefined`).
  - `Cache.del(key)` returns `boolean`.
  - `Cache.setEx` (camelCase), `Cache.getJSON<T>`, `Cache.setJSON<T>` added.
  - `cache.raw` escape hatch exposes the underlying `RecachedCache` for advanced use.
- `pkg/recached_edge.d.ts` stub committed to the repository so `tsc` type-checks the SDK on a fresh checkout without requiring a prior `wasm-pack build`.
- `tsconfig.json` added; `npm run build` runs `wasm-pack build --target web --out-dir pkg && tsc`.
- `package.json` updated with `"type": "module"`, `"main"`, `"types"`, `"exports"`, and `"files"` fields.

---

## [0.1.2] — 2026-05-02

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
