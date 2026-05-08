# Changelog

All notable changes to Recached are documented here.

---

## [0.1.4] — 2026-05-09

### Added

**Snapshot persistence** (`server-native`)
- New `SAVE` command: blocks until the snapshot is written to disk and returns `+OK`.
- New `BGSAVE` command: forks a background Tokio task to write the snapshot and immediately returns `+Background saving started`. The server continues accepting connections during the save.
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
