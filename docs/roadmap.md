# Roadmap

Recached competes on **where the data can live** — the same engine on the server and in the browser, with sync in between. The [benchmarks](/guide/benchmarks) show this costs nothing in raw speed.

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


## 5. Offline-first writes with merge semantics ✅ shipped

Browser clients queue writes as *operations* in a **durable outbox** while offline (IndexedDB-backed with persistence enabled, so they survive a full page reload), auto-reconnect with backoff, re-establish the session (auth, sync token, live queries — which re-hydrate local state), and replay the outbox. Writes are retired only on server acknowledgment (at-least-once delivery). Operation replay makes merges type-aware: `incr`/`decr` queue deltas that merge additively (PN-counter semantics), `jmerge` patches deep-merge, collection ops replay, and plain `set` is last-writer-wins by server arrival. See [Offline & Reconnection](/browser/offline).


## 6. Mobile SDKs — React Native, Flutter, Kotlin, Swift

- **Kotlin + Swift first**, via a single `uniffi`-annotated Rust crate that generates bindings for both. The platform WebSocket (OkHttp / URLSession) feeds frames into `sync-client` — no embedded async runtime. Persistence: a file/SQLite adapter over the same outbox/meta effects the browser maps to IndexedDB. Reactivity: Kotlin `Flow` / Swift `Observation` over keychange pushes.
- **Flutter** via `flutter_rust_bridge`: synchronous local reads into Rust memory, `watchKey()` → `Stream` for rebuilds.
- **React Native** last (Hermes has no WASM): `uniffi-bindgen-react-native` reuses the same binding layer, and the existing React hooks API carries over — same `useKey` in React DOM and React Native.


## 7. WASM server-side scripting

Run `.wasm` stored procedures in place of Lua scripts. The scripting VM would be sandboxed (no network, no file I/O, bounded execution time), accept any WASM module that exports a specific entry function, and execute it against the cache store. Supports any language that compiles to WASM: Rust, Go (TinyGo), AssemblyScript, Python.


## 8. WASI target

A `wasm32-wasip1` build of `wasm-edge` for Cloudflare Workers and Deno Deploy, running Recached as a cache layer at the edge with the same API as the browser client.

`core-engine` is already `wasm32`-compatible; the work is adapting the WebSocket and persistence layers to WASI. Last on the list because the platform fights the model — Workers cannot hold persistent WebSockets outside Durable Objects — and edge platforms ship native KV stores.

---

## AI-era features

Recached's unfair advantage is *where the data lives* — so the winning AI features put the intelligence layer **next to the user** instead of behind another network hop. Ordered by intended sequence.

### 9. Token-cost rate limiting

AI providers meter tokens, not requests. One optional argument extends the existing limiter to weighted budgets:

```bash
RLCHECK user:42 100000 3600 COST 1850   # consume 1,850 tokens of a 100k/hour budget
```


### 10. Semantic caching (`SEMSET` / `SEMGET`)

LLM calls are expensive and repeats are *paraphrases*, so exact-key caching misses them. A semantic cache returns a hit when a query's embedding is close enough to a cached one:

```bash
SEMSET prompts <embedding> "<cached LLM response>" EX 3600
SEMGET prompts <embedding> 0.92          # → cached response or nil
```

### 11. Streaming values — "watch the agent think"

An agent streams tokens into a key with `APPEND`; every subscribed browser renders it live. Live queries already deliver the subscription — the missing piece is an append *delta* frame (keychange currently re-sends the whole value) plus catch-up-then-follow on reconnect. `useKey('agent:run:42:output')` becomes a live-typing agent visible to any number of viewers. Redis Streams end at the backend; this reaches the UI.

### 12. Vector search in the browser

Server-side vector search is table stakes now (Redis 8 has it). What only Recached's architecture allows: the same vector index compiled to WASM and synced via scoped sync — **on-device semantic search over the user's own data, offline, zero-latency**. Personal RAG memory that works on a plane. A server-only tool structurally cannot copy this.

### 13. Computed keys — the reactive cache

Declare a key as a function of other keys; the server recomputes on change and the diff flows through live queries — cache becomes spreadsheet. `cart:42:total` recomputes when any `cart:42:item:*` changes, and every subscribed UI updates. Uses WASM scripting (#7) as the function runtime. Biggest lift, biggest ceiling.

Under consideration behind these: a CRDT text type for collaborative editing (likely embedding an existing Rust CRDT rather than building one), and per-key undo/history on top of the existing op-log machinery.

---

## Ongoing: drop-in credibility

Not features, but continuous work that keeps "any Redis client works today" honest:

- **Binary-safe WebSocket values** — values over WS are currently UTF-8 (lossy for raw bytes); binary frames would make the two transports equivalent.
- **RESP3** — push protocol support on the TCP port.
- **Command coverage** — closing gaps in the supported command set as real workloads surface them (see [Commands](/server/commands)).

---

Feedback on priorities is welcome — [open an issue](https://github.com/thinkgrid-labs/recached/issues) or write to [dennis@thinkgrid.dev](mailto:dennis@thinkgrid.dev).
