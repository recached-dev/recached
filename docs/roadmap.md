# Roadmap

Recached competes on **where the data can live** — the same engine on the server and in the browser, with sync in between. The [benchmarks](/guide/benchmarks) show this costs nothing in raw speed. Every item below either sharpens that differentiator or removes a blocker to using it in production.

Ordered by priority.

## 1. Rate-limiting commands

`RLSET key limit window` / `RLCHECK key`. A built-in sliding-window rate limiter that replaces hand-rolled INCR+EXPIRE (racy) or Lua script approaches. The window is stored as a sorted set internally; the API is a single command.

## 2. Scoped sync and per-client auth

Today every mutation fans out to **every** connected WebSocket client. For a multi-user application that is a data-leak footgun: user A's session keys are pushed to user B's browser. Before the browser story can be used in serious production:

- Clients subscribe to key prefixes or patterns (`sync: ['cart:{userId}:*', 'catalog:*']`) instead of receiving everything.
- Scopes are authorized server-side (per-connection token → allowed patterns), so a client cannot subscribe to keys it shouldn't see.
- Fan-out filters by scope, which also cuts broadcast cost — most mutations stop being everyone's problem.


## 3. Live queries — "Redis that renders"

Extend the existing keychange push into pattern subscriptions with initial state. A React component does:

```tsx
const cart = useKeys('cart:item:*'); // current matching keys + live updates
```

and gets the full loop with zero application glue: server write → patch over WebSocket → local WASM cache → component re-render. Reads stay local (0 ms); the subscription delivers initial state plus diffs, not fire-and-forget events.


## 4. Native JSON type

`JSET key path value`, `JGET key path`, `JMERGE key patch`. JSONPath-based access to nested JSON structures stored as a native type, without RedisJSON. Avoids the serialize-deserialize round-trip for complex objects where only part of the document changes.


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
