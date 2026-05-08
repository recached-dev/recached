# Roadmap

## Planned

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
