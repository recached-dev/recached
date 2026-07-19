# Roadmap

Recached competes on **where the data can live** — the same engine on the server and in the browser, with sync in between. The [benchmarks](/guide/benchmarks) show this costs nothing in raw speed.

## Near-term

**[Byte-slice command arguments](#performance)** is the one substantial piece left in the hardening
list, and the main remaining lever on unpipelined latency. It is a large mechanical refactor — 125
parse arms, ~210 argument extractions — and its payoff needs a quiet machine to measure, so it wants
its own focused pass rather than being folded into a release alongside other work.

After that, in order:

1. **[Replication lag](#operability)** — replica *count* is exported, but not how far behind each one
   is, which is the signal that matters before a failover.
2. **[Bound rate-limiter memory](#operability)** — one timestamp per attempt means ~800 KB for a
   single `RLSET key 100000 3600` limiter.
3. **[Make the compiled-in limits configurable](#operability)** — outbox size, live queries per
   connection, eviction sample size.

---

## 6. Mobile SDKs — React Native, Flutter, Kotlin, Swift

- **Kotlin + Swift first**, via a single `uniffi`-annotated Rust crate that generates bindings for both. The platform WebSocket (OkHttp / URLSession) feeds frames into `sync-client` — no embedded async runtime. Persistence: a file/SQLite adapter over the same outbox/meta effects the browser maps to IndexedDB. Reactivity: Kotlin `Flow` / Swift `Observation` over keychange pushes.
- **Flutter** via `flutter_rust_bridge`: synchronous local reads into Rust memory, `watchKey()` → `Stream` for rebuilds.
- **React Native** last (Hermes has no WASM): `uniffi-bindgen-react-native` reuses the same binding layer, and the existing React hooks API carries over — same `useKey` in React DOM and React Native.


## 7. WASM server-side scripting

Run `.wasm` stored procedures in place of Lua scripts. The scripting VM would be sandboxed (no network, no file I/O, bounded execution time), accept any WASM module that exports a specific entry function, and execute it against the cache store. Supports any language that compiles to WASM: Rust, Go (TinyGo), AssemblyScript, Python.


## 8. WASI target

A `wasm32-wasip1` build of `wasm-edge` for Cloudflare Workers and Deno Deploy, running Recached as a cache layer at the edge with the same API as the browser client.

`core-engine` is already `wasm32`-compatible; the work is adapting the WebSocket and persistence layers to WASI. Last on the list because the platform fights the model — Workers cannot hold persistent WebSockets outside Durable Objects — and edge platforms ship native KV stores.


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


### 12. Computed keys — the reactive cache

Declare a key as a function of other keys; the server recomputes on change and the diff flows through live queries — cache becomes spreadsheet. `cart:42:total` recomputes when any `cart:42:item:*` changes, and every subscribed UI updates. Uses WASM scripting (#7) as the function runtime. Biggest lift, biggest ceiling.

Under consideration behind these: a CRDT text type for collaborative editing (likely embedding an existing Rust CRDT rather than building one), and per-key undo/history on top of the existing op-log machinery.

---

## Hardening & enhancements

Improvements to shipped functionality rather than new surface. Unnumbered because they are small and
independent — pick them off in any order.

### Reliability

### Live queries

### Performance

Both of these are diagnosed on the [benchmarks](/guide/benchmarks) page; the analysis is done, the
work is not.

**Byte-slice command arguments.** RESP parsing allocates a `String` per argument. This is the main
remaining lever on unpipelined latency and the likeliest explanation for `HSET` sitting at ~46 % of
Redis while *beating* it pipelined.

**Serialize `LRANGE` straight from the store**, instead of building the full reply `Value` first.

### Operability

**Replication lag.** The number of connected replicas is exported, but not how far behind each one
is — the signal that actually matters before a failover.

**Make the compiled-in limits configurable.** The 10 000-write outbox, 64 live queries per
connection, and eviction's fixed 10-key sample are all constants. Redis exposes `maxmemory-samples`
for the same reason: the right value is workload-dependent.

**Bound rate-limiter memory.** The limiter stores one timestamp per attempt, so `RLSET key 100000
3600` holds 100 000 `u64`s — roughly 800 KB for a single key. The cost-weighted rework in
[#9](#_9-token-cost-rate-limiting) is the natural moment to move to bucketed counts.

### Security

**Warn on sync tokens minted without an expiry.** Expiry is optional in the token payload, so a token
issued without one is valid forever. Refusing to mint it — or at minimum logging loudly — would match
how half-configured TLS is now handled.

**Per-command ACLs and an audit log.** Both are named as gaps in the
[threat model](/server/security#threat-model-stated-plainly): authentication on the RESP port is
all-or-nothing, and there is no record of who read or wrote what. Neither is interesting engineering;
both are procurement checkboxes worth building when someone actually asks.

---

## Ongoing: drop-in credibility

Not features, but continuous work that keeps "any Redis client works today" honest:

- **Binary-safe WebSocket values** — values over WS are currently UTF-8 (lossy for raw bytes); binary frames would make the two transports equivalent.
- **RESP3** — push protocol support on the TCP port.
- **Command coverage** — closing gaps in the supported command set as real workloads surface them (see [Commands](/server/commands)).

---

Feedback on priorities is welcome — [open an issue](https://github.com/thinkgrid-labs/recached/issues) or write to [dennis@thinkgrid.dev](mailto:dennis@thinkgrid.dev).
