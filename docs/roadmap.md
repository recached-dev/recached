# Roadmap

Recached competes on **where the data can live** — the same engine on the server and in the browser, with sync in between. The [benchmarks](/guide/benchmarks) show this costs nothing in raw speed. Every item below either sharpens that differentiator or removes a blocker to using it in production.

Ordered by priority.

## 1. Rate-limiting commands ✅ shipped

`RLSET key limit window` / `RLCHECK key [limit window]`. A built-in sliding-window rate limiter that replaces hand-rolled INCR+EXPIRE (racy) or Lua script approaches. `RLCHECK` returns `[allowed, remaining, retry_after_ms]` — a direct fit for `X-RateLimit-*` / `Retry-After` headers — and the inline config form auto-creates self-cleaning per-IP/per-user limiters in a single command. See [Commands → Rate Limiting](/server/commands#rate-limiting).

## 2. Scoped sync and per-client auth ✅ shipped

Every WebSocket connection can now be scoped to glob patterns via the `SYNC` command, and the mutation fan-out delivers only matching keys. With `RECACHED_SYNC_SECRET` set, scopes become a real authorization boundary: connections present an HMAC-signed token minted by your backend (`SYNC TOKEN <token>`), and every command — reads included — is checked against the granted patterns; admin/keyspace-wide commands are refused. See [Sync Scopes](/server/sync-scopes).


## 3. Live queries — "Redis that renders" ✅ shipped

`QSUB pattern` returns the current state of every matching key and then streams keychange diffs — initial state plus diffs, not fire-and-forget events — scope-checked under strict sync scoping. The client half makes it a one-liner in React and Vue:

```tsx
const cart = useKeys('cart:item:*'); // current matching keys + live updates
```

Server write → diff over WebSocket → local WASM cache → component re-render, with zero application glue. See [Commands → Live Queries](/server/commands#live-queries-websocket-only) and [`useKeys`](/react/hooks-reference#usekeys-pattern).


## 4. Native JSON type ✅ shipped

`JSET key path value`, `JGET key [path]`, `JMERGE key patch` — nested JSON stored as a native type, without RedisJSON. Path reads and partial updates never re-serialize the whole document, and only the change travels over the wire. `JMERGE` follows RFC 7386 (deep merge, `null` removes fields). The browser SDK mirrors all three (`jset`/`jget`/`jmerge`), so a merge from any client updates every connected browser's local document. See [Commands → JSON](/server/commands#json).


## 5. Offline-first writes with merge semantics

Browser clients already persist through IndexedDB and read locally. The missing piece is writing while offline: queue mutations locally, reconcile on reconnect.

- Default policy: last-write-wins with server timestamps.
- CRDT semantics where the data type makes them natural: `INCR`/`DECR` as a PN-counter (offline increments merge additively instead of clobbering), `SADD`/`SREM` as an observed-remove set.

## 6. Mobile SDKs — React Native, Flutter, Kotlin, Swift


- **Kotlin + Swift first**, via a single `uniffi`-annotated Rust crate that generates bindings for both. The platform WebSocket (OkHttp / URLSession) feeds RESP frames into the engine's existing parser — no embedded async runtime. Persistence reuses the existing snapshot format as a file. Reactivity: Kotlin `Flow` / Swift `Observation` over keychange pushes.
- **Flutter** via `flutter_rust_bridge`: synchronous local reads into Rust memory, `watchKey()` → `Stream` for rebuilds.
- **React Native** last (Hermes has no WASM): `uniffi-bindgen-react-native` reuses the same binding layer, and the existing React hooks API carries over — same `useKey` in React DOM and React Native.


## 7. WASM server-side scripting

Run `.wasm` stored procedures in place of Lua scripts. The scripting VM would be sandboxed (no network, no file I/O, bounded execution time), accept any WASM module that exports a specific entry function, and execute it against the cache store. Supports any language that compiles to WASM: Rust, Go (TinyGo), AssemblyScript, Python.


## 8. WASI target

A `wasm32-wasip1` build of `wasm-edge` for Cloudflare Workers and Deno Deploy, running Recached as a cache layer at the edge with the same API as the browser client.

`core-engine` is already `wasm32`-compatible; the work is adapting the WebSocket and persistence layers to WASI. Last on the list because the platform fights the model — Workers cannot hold persistent WebSockets outside Durable Objects — and edge platforms ship native KV stores.

---

## Ongoing: drop-in credibility

Not features, but continuous work that keeps "any Redis client works today" honest:

- **Binary-safe WebSocket values** — values over WS are currently UTF-8 (lossy for raw bytes); binary frames would make the two transports equivalent.
- **RESP3** — push protocol support on the TCP port.
- **Command coverage** — closing gaps in the supported command set as real workloads surface them (see [Commands](/server/commands)).

---

Feedback on priorities is welcome — [open an issue](https://github.com/thinkgrid-labs/recached/issues) or write to [dennis@thinkgrid.dev](mailto:dennis@thinkgrid.dev).
