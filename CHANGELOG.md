# Changelog

All notable changes to Recached are documented here.

---

## [0.1.8] — 2026-07-09

### Fixed

**Performance**
- `SPOP` and `SRANDMEMBER` were O(n) in set size per call: random members were selected by iterating (and for `SPOP`, cloning) every member of the set. On a 100k-member set `SPOP` managed ~800 ops/sec. Sets are now backed by `IndexSet` instead of `HashSet`, giving O(1) random access by index and O(1) `swap_remove` — `SPOP`/`SRANDMEMBER` now cost O(k) in the number of members requested, independent of set size. Snapshot format is unchanged. (`core-engine/src/store.rs`)
- Every write paid for sync machinery it often didn't need: the RESP push message for WebSocket sync was built (and cloned) even with zero browser clients connected; `on_write` locked the global replica registry even with no replicas and no AOF; the watch registry's global mutex was locked on every mutation even with nothing watched; and `broadcast_for` ran twice per write (once at the call site, once inside `notify_watchers`). The post-write fan-out is now consolidated in `apply_write_effects`, gated by atomic counters — with no WS clients, no replicas, no AOF, and no watched keys, a write skips all of it: zero locks, zero allocations, one `broadcast_for` at most. (`server-native/src/main.rs`)
- Command execution hot path, three fixes: **(a)** every command did 1–2 metrics-registry lookups per execution (`counter!` key construction + a lock in the global recorder) — under 8 worker threads this contention was the dominant per-command cost and the cause of pipelined throughput decaying within seconds of sustained load, with stalls up to 3 s; counter handles are now resolved once and cached (`record_command`, cached `KEYSPACE_HITS`/`KEYSPACE_MISSES`). **(b)** `execute_and_record` cloned every `Command` before execution; it now takes the command by value and callers clone only when a write-effect consumer (WebSocket peer, replica, AOF, watched key) actually exists. **(c)** `Value::serialize` allocated a fresh `Vec` per response — and one per array element — per command; the new `Value::serialize_into` encodes in place and the TCP handler reuses one response buffer per connection. Combined result on a 4-core i5 (`redis-benchmark -P 16`, suite run): SET 200.8k → 421.9k, GET 33.9k → 546.4k, INCR 13.5k → 448.4k, LPUSH 9.8k → 473.9k, SADD 30.0k → 421.9k, HSET 25.5k → 408.2k, ZADD 26.2k → 414.9k ops/sec — ahead of Redis 7.2.5 on 6 of 7 pipelined commands and ahead of Valkey 9.1.0 on all 7, with the multi-second stalls gone (suite p99 ≤ 5.1 ms). Unpipelined: GET 38.9k → 58.1k, LPUSH 27.1k → 49.1k, SADD 31.1k → 51.6k, MSET 24.9k → 34.5k ops/sec. (`server-native/src/main.rs`, `core-engine/src/resp.rs`)

### Added

- `scripts/benchmark.sh` — reproducible `redis-benchmark` suite used for the published numbers; docs gained a Benchmarks page (`docs/guide/benchmarks.md`) comparing against Redis 7.2.5 and Valkey 9.1.0.
- `recached-server --version` / `-V` prints the version and exits. Previously the flag was ignored and the server booted — which also made the Homebrew formula's `test do` block hang. The formula now installs per-architecture binaries (`on_intel` / `on_arm`) for v0.1.8. (`server-native/src/main.rs`, `Formula/recached.rb`)

---

## [0.1.7] — 2026-06-12

### Fixed

**Security**
- Replication auth password was compared with `!=` (byte-by-byte), leaking timing information an attacker could use to brute-force the password one character at a time. Replaced with a constant-time XOR-fold comparison. (`server-native/src/main.rs`)
- The client `AUTH` command compared the supplied password with `==` (`String` equality, short-circuiting on the first mismatched byte) — the same timing side-channel the replication path was already hardened against. `process_auth` now uses the constant-time comparison. (`server-native/src/main.rs`)
- Replication frame length prefixes (snapshot and per-command) were read and allocated without an upper bound. Because the replication port may be unauthenticated and plaintext, a peer or MITM could send a 4 GB length prefix and force a matching allocation per frame (memory DoS). Frames are now capped at 512 MB before allocation. (`server-native/src/main.rs`)
- `auth()` in the WASM SDK now emits a `console.warn` when the active connection is an unencrypted `ws://` URL, alerting developers that the password is sent in plaintext. Production deployments should use `wss://`. (`wasm-edge/src/lib.rs`)

**Correctness**
- WebSocket `connect()` followed by `auth()` raced the socket handshake: `createCache` sent `AUTH` (and any early `set`/`del`/`subscribe`/`publish`) while the socket was still `CONNECTING`, so the frames were silently dropped — server sync was completely broken whenever `RECACHED_PASSWORD` was set, and early writes were lost otherwise. Commands issued before the socket opens are now buffered and flushed in FIFO order by an `onopen` handler. (`wasm-edge/src/lib.rs`, `wasm-edge/sdk.ts`)
- `MULTI`/`EXEC` did not honour `WATCH`: a watched key changing before `EXEC` did not abort the transaction, so the standard Redis optimistic-locking (compare-and-swap) pattern silently lost updates. `EXEC` now returns a nil array when any watched key has changed since `WATCH`; `EXEC` and `DISCARD` clear all watches; and `WATCH`/`UNWATCH` inside `MULTI` are rejected. Works over both the TCP and WebSocket ports. (`server-native/src/main.rs`)
- AOF replay restored nothing on the live server. Writes are recorded via `on_write` in RESP3 Push (`>`) form, but `replay_aof` passed parsed frames straight to `Command::from_value`, which only accepts arrays — so every replayed frame was rejected and skipped (the existing test masked this by feeding `*`-array frames). Replay now normalises Push→Array, matching the replica stream path. (`server-native/src/main.rs`)
- `SPOP` and `SRANDMEMBER` returned members in `HashMap` iteration order rather than randomly, and positive `SRANDMEMBER count` was non-random while negative count was fully deterministic (`members[i % len]`). All now sample randomly, matching Redis. (`core-engine/src/store.rs`)
- `allkeys-lru` / `volatile-lru` eviction ranked entries by last *write* time and never updated it on reads, so a hot, frequently-read key could be evicted as if it were cold. Entries now carry an atomic last-access timestamp refreshed on the main read paths (`GET`, `MGET`, `HGET`/`HGETALL`, `LRANGE`, `SMEMBERS`, `SISMEMBER`, `ZSCORE`, and the sorted-set range reads), giving true access-based LRU. (`core-engine/src/store.rs`)
- `SCAN` ignored its `COUNT` argument and returned the entire matching keyspace in one reply at cursor `0`, defeating its purpose as the non-blocking alternative to `KEYS`. It now returns at most `COUNT` keys per call (default 10) with a real next-cursor for incremental iteration. (`core-engine/src/store.rs`)
- A read-only replica applied writes streamed from the primary but never re-broadcast them, so the replica's own WebSocket clients received no live updates and multi-tier (chained) replication was impossible. Replicas now relay each applied write to their local WebSocket clients and run a replication server so they can serve sub-replicas. (`server-native/src/main.rs`)
- Local writes through the SDK fired the mutation callback twice — once from the Rust layer and again in `sdk.ts` — causing redundant `useSyncExternalStore` re-renders. The duplicate notification was removed. (`wasm-edge/sdk.ts`)
- `SRANDMEMBER key -N` panicked with a divide-by-zero when the target key did not exist or the set was empty. An early-return guard now produces an empty array, matching Redis semantics. (`core-engine/src/store.rs`)
- `ZINCRBY` did not validate the resulting score for NaN or Infinity before writing it into the sorted set, corrupting subsequent range queries when called with `+inf`/`-inf` deltas. The result is now pre-computed and rejected with `ERR increment would produce NaN or Infinity` if invalid — consistent with `HINCRBYFLOAT`. (`core-engine/src/store.rs`)
- `DECRBY` used `extract_string` (no size limit) for key parsing while `INCRBY` used `extract_key` (≤ 512 KB). Keys larger than 512 KB sent via `DECRBY` now return an error, consistent with all other key-bearing commands. (`core-engine/src/cmd.rs`)
- `SET … EX <n>` with a TTL value large enough to overflow `u64` when multiplied by 1 000 silently saturated to `u64::MAX`, making the key effectively immortal. Such values now return `ERR TTL overflow`. (`core-engine/src/store.rs`)
- Vue `useKey` and `useKeyJSON` read the initial value before subscribing to mutations, leaving a narrow window where a write between `get()` and `onMutation()` was missed. The subscription is now registered first, then the initial value is read. (`recached-vue/src/useKey.ts`)
- React `usePubSub` captured the `handler` closure at subscribe time and held it for the lifetime of the effect. Inline handlers (redefined each render) would go stale and never receive updated closure state. The hook now stores the latest handler in a `useRef` and calls through it — no re-subscribe needed when the handler changes. (`recached-react/src/usePubSub.ts`)

**Performance**
- Memory-limit eviction (`RECACHED_MAX_MEMORY`) was O(N²): `try_evict_for_memory` re-scanned the entire keyspace to recompute total memory after every single eviction, on the 1-second background sweep — stalling the server under exactly the memory pressure it was meant to relieve. It now measures total memory once and maintains it incrementally by subtracting each evicted entry's measured size, with a periodic re-sync to correct drift. (`core-engine/src/store.rs`)

**DoS / resilience**
- The RESP array parser bounded each bulk string at 64 MB but applied no limit to the total number of elements, making it possible to stream 1 million small strings and force ~64 TB of cumulative allocation before rejection. A 64 MB cumulative-bytes check is now applied across the entire array parse loop. (`core-engine/src/resp.rs`)
- The RESP3 Push (`>`) parser lacked the cumulative-size guard the array parser has, so the replica and AOF parse paths would accept arbitrarily large push frames. The same 64 MB cumulative check is now applied. (`core-engine/src/resp.rs`)
- The glob matcher used by `KEYS` and `SCAN` was a recursive function with no memoization or depth limit. Patterns such as `*.*.*.*x` against a long non-matching string caused exponential backtracking (ReDoS). The implementation is replaced with an iterative two-row DP algorithm that is strictly O(m × n). (`core-engine/src/store.rs`)

**Portability**
- On non-Unix platforms the server bound one listener socket per CPU core to the same port, relying on `SO_REUSEPORT` (Unix-only). Without it the second bind failed and the process exited at startup. Non-Unix builds now fall back to a single accept loop. (`server-native/src/main.rs`)

**Resource management**
- Calling `connect()` a second time on a `RecachedCache` instance replaced the internal `WebSocket` field without closing the previous socket. The old connection remained open, receiving stale messages. `connect()` now calls `.close()` on the existing socket before creating the new one. (`wasm-edge/src/lib.rs`)

### Added

- **`RECACHED_BIND`** — new env var controlling the network interface every listener (TCP, WebSocket, replication, metrics) binds to. Defaults to `0.0.0.0` for backwards compatibility; set `127.0.0.1` (or a specific private interface) to keep the server off public interfaces. A startup warning is logged when bound to all interfaces. (`server-native/src/main.rs`)
- **`WATCH` / `UNWATCH` over TCP** — optimistic-lock (CAS) `WATCH` is now available on the RESP/TCP port (6379), not just WebSocket. TCP clients receive no keychange push (it would break the request/response protocol); they use `WATCH` purely for the `EXEC` abort guarantee. (`server-native/src/main.rs`)

### Changed

- `WATCH`/`UNWATCH` semantics: previously WebSocket-only "observable keys" with no transactional effect, they now provide Redis-compatible optimistic locking on both transports. Over WebSocket, `WATCH` additionally pushes live keychange notifications as before. The store no longer returns `ERR WATCH/UNWATCH only supported over WebSocket`. (`server-native/src/main.rs`, `docs/server/commands.md`)
- The replication server now runs on every node, including replicas, so a replica can in turn serve sub-replicas (multi-tier replication). (`server-native/src/main.rs`)
- The `Entry` struct's `written_at_ms` field was replaced with an atomic `last_access_ms`, refreshed on reads, to back access-based LRU eviction. (`core-engine/src/store.rs`)
- Documented that WebSocket uses text frames, so values must be valid UTF-8 (non-UTF-8 bytes are replaced lossily); raw binary values are fully round-trippable only over the TCP port. The SDK's string-typed `set` API is unaffected. (`server-native/src/main.rs`)

---

## [0.1.6] — 2026-05-11

### Fixed

**Correctness**
- `TTL` / `PTTL`: replaced `exp - now` with `exp.saturating_sub(now)` — a tight race between the expiry check and the subtraction could panic in debug builds or wrap to `u64::MAX` in release, returning a wildly incorrect TTL. (`core-engine/src/store.rs`)
- `DEL` / `UNLINK`: switched from `data.remove(k)` to `data.remove_if(k, |_, e| !e.is_expired(now))` — expired-but-not-yet-swept keys were counted as deleted, violating Redis semantics which returns 0 for missing/expired keys. (`core-engine/src/store.rs`)
- `ZADD GT` / `LT` flags were parsed and silently discarded. They are now fully enforced: `GT` updates an existing member only if the new score is greater; `LT` only if lower; new members are always inserted regardless of the flag. Incompatible combinations (`GT`+`LT`, `GT`/`LT`+`NX`) return errors matching Redis. (`core-engine/src/cmd.rs`, `core-engine/src/store.rs`)

**Security**
- `Command::Auth` reached `store.execute()` and unconditionally returned `+OK`, bypassing authentication during AOF replay and any other path that calls the store directly. `store.execute()` now returns an error for `Auth` — authentication is handled exclusively by the connection-layer `process_auth` function. (`core-engine/src/store.rs`)

**Performance / reliability**
- `PubSubHub::unsubscribe` left an empty `Vec` in `channel_subs` after the last subscriber left a channel. Over time, high-churn subscriber patterns leaked memory proportional to the total number of unique channels ever seen. Empty entries are now removed immediately in `unsubscribe`, `unsubscribe_all`, and `publish`. (`server-native/src/main.rs`)
- `SharedPubSub` and `WatchRegistry` used `std::sync::Mutex` (blocking) in async connection handlers. Holding a blocking lock across `.await` points starves the Tokio thread pool under high pub/sub publish rates. Both types now use `tokio::sync::Mutex`; `notify_watchers` is now `async`. (`server-native/src/main.rs`)

### Added

- Key-length validation in `Command::from_value`: keys larger than 512 KB or empty keys are rejected at parse time with a descriptive `ERR` before reaching the store. Validation is applied to all primary-key positions in `GET`, `SET`, `DEL`, `UNLINK`, `MGET`, `MSET`, `EXISTS`, `APPEND`, `STRLEN`, `GETSET`, `SETNX`, `SETEX`, `PSETEX`, `INCR`, `DECR`, `INCRBY`, and all commands that assign `let key = …`. (`core-engine/src/cmd.rs`)

### Changed

- `format_score` (f64 → Redis score string) is now `pub` and exported from `core-engine::store`. The identical private `format_zset_score` function in `wasm-edge` has been removed in favour of the shared implementation. (`core-engine/src/store.rs`, `wasm-edge/src/lib.rs`)

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
