# Roadmap

## What shipped

The following features are implemented and stable.

**Protocol & server**

- RESP protocol — full parser and serializer, fragmentation handling, depth-limited (no stack-overflow DoS)
- TCP server (port 6379) — compatible with any Redis client (`ioredis`, `node-redis`, `redis-py`, `Jedis`, etc.)
- WebSocket sync (port 6380) — real-time mutation broadcast between server and browser WASM instances
- Sender-ID dedup filter — browser clients do not double-apply their own mutations
- `RECACHED_PASSWORD` + brute-force lockout after 5 consecutive failed `AUTH` attempts
- `RECACHED_ALLOW_IPS` — comma-separated IP allowlist with validated IP parsing
- `RECACHED_MAX_KEYS` — hard key cap
- Connection semaphore (max 1024 concurrent connections)
- Eviction policies: `noeviction`, `lru`, `allkeys-random`, `volatile-lru`, `volatile-ttl`
- Background active eviction (1s sweep) + lazy eviction on every read
- TLS on both ports (`RECACHED_TLS_CERT` + `RECACHED_TLS_KEY`)
- Prometheus metrics (`RECACHED_METRICS_PORT`, scrape at `/metrics`)
- Structured `tracing` logs with configurable level via `RUST_LOG`
- Docker image (`ghcr.io/thinkgrid-labs/recached`)
- Homebrew formula

**Commands**

See [Commands](/server/commands) for the full list. In summary: `PING`, `AUTH`, all String commands, Expiry commands, Key management (`DEL`, `EXISTS`, `TYPE`, `RENAME`, `KEYS`, `SCAN`, `DBSIZE`, `FLUSHDB`), Hash, List, Set, Sorted Set, Transactions (`MULTI`/`EXEC`/`DISCARD`), Pub/Sub (`SUBSCRIBE`, `UNSUBSCRIBE`, `PSUBSCRIBE`, `PUNSUBSCRIBE`, `PUBLISH`), and WebSocket-only observable keys (`WATCH`/`UNWATCH`).

**Browser (WASM)**

- `recached-edge` npm package — TypeScript SDK for the browser
- `RecachedCache` class — zero-latency local reads, all cache types
- WebSocket sync — connect to port 6380 and receive server mutations automatically
- Observable keys (`cache.watch()`) — callbacks on key change from any source
- Pub/Sub over WebSocket — `subscribe()` and `publish()` in the browser
- IndexedDB WAL persistence — cache survives page refresh
- BroadcastChannel cross-tab sync — all tabs in the same origin share mutations

---

## Planned

### Delta sync (`SYNC <seq>`)

Currently, when a WebSocket client reconnects after a gap, the server performs a full resync: it sends the current value of all keys. For most applications this is fine, but for high write-rate deployments with large key sets, a full resync on every reconnect adds unnecessary data transfer.

The plan: the server maintains an in-memory ring buffer of recent mutations (configurable depth, e.g. last 10,000 writes). Each mutation has a sequence number. On reconnect, the client sends `SYNC <last-seq>` and the server replays only the mutations that occurred after that sequence number. If the client's sequence number is older than the ring buffer, the server falls back to a full resync.

The `wasm-edge` module already tracks a WAL sequence number — this becomes the cursor for delta sync.

### WASI target

A `wasm32-wasip1` build of `wasm-edge` for Cloudflare Workers and Deno Deploy. This would allow the Recached WASM module to run at the edge as a cache layer between origin and CDN, with the same API as the browser client.

The `core-engine` crate is already `wasm32`-compatible. The main work is adapting the WebSocket and persistence layers for the WASI environment.

### Native JSON type

`JSET key path value`, `JGET key path`, `JMERGE key patch`. JSONPath-based access to nested JSON structures stored as a native type, without RedisJSON. This avoids the serialize-deserialize overhead for complex objects where only a part of the document changes frequently.

### Rate-limiting commands

`RLSET key limit window` / `RLCHECK key`. A built-in sliding-window rate limiter that replaces hand-rolled INCR+EXPIRE or Lua script approaches. The window is stored as a sorted set internally; the API is a single command.

### WASM server-side scripting

Run `.wasm` stored procedures in place of Lua scripts. The scripting VM would be sandboxed (no network, no file I/O, bounded execution time), accept any WASM module that exports a specific entry function, and execute it against the cache store. This supports any language that compiles to WASM: Rust, Go (TinyGo), AssemblyScript, Python (via Pyodide).

---

## Intentionally out of scope

These features will not be added to Recached. If you need them, Redis is the right tool.

**RDB / AOF persistence.** Recached is an in-memory cache. Durability is the responsibility of the database behind it. Repopulate on startup from your source of truth.

**`REPLICAOF` / leader-follower replication.** The native→browser WebSocket sync is Recached's replication story. Multi-server Redis-style replication does not fit the architecture.

**Full Redis command parity.** Recached implements ~80 commands — the ones most applications actually use. The remaining 170+ Redis commands include cluster management, server introspection (`INFO`, `SLOWLOG`, `DEBUG`), and commands that assume RDB persistence (`BGSAVE`, `BGREWRITEAOF`, `SAVE`). These are not planned.

**RESP3.** RESP2 is sufficient for Recached's scope and keeps the parser simple. RESP3 adds type hints that matter more for complex Redis use cases than for Recached's.

**Cluster mode.** Recached is a single-node cache server. Horizontal scaling is not a goal for the current architecture.

**Lua scripting.** WASM scripting (see roadmap above) is the planned scripting story. Lua will not be added.
