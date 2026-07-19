# Roadmap

Recached competes on **where the data can live** — the same engine on the server and in the browser, with sync in between. The [benchmarks](/guide/benchmarks) show this costs nothing in raw speed.

Numbered items are stable identifiers, not an ordering — code and changelog entries reference them
(e.g. "roadmap #6"), so they are never renumbered. **Near-term priorities** are called out below.

Items 1–5 have shipped: rate-limiting commands, scoped sync with per-client auth, live queries, the native JSON type, and offline-first writes with merge semantics. Their numbering is retained below so existing references stay valid — see the [changelog](https://github.com/thinkgrid-labs/recached/blob/main/CHANGELOG.md) for what landed in each.

## Near-term

The next three things worth doing, in order:

1. **[Reconnect backoff jitter](#reliability)** — a few lines; prevents a thundering
   herd every time the server restarts.
2. **[Presence — connection-scoped keys](#_13-presence-—-connection-scoped-keys)** — the docs already
   sell presence as a use case; the primitive does not exist yet.
3. **[Live queries carry collection values](#live-queries)** — removes a
   round-trip that currently contradicts the local-read premise.

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

## 13. Presence — connection-scoped keys

Keys whose lifetime is tied to the connection that set them. The server drops them when the socket
closes, and the deletion fans out through live queries like any other change.

```bash
ESET presence:room:7:user:42 "typing"   # lives exactly as long as this connection
```

Today presence has to be hand-rolled with `SETEX` plus a heartbeat, which leaves ghost entries for
the length of the TTL whenever a tab closes — the classic "still shows as online" bug. Every piece
needed already exists: the WebSocket handler knows when a connection ends, and `QSUB` already
broadcasts deletions.

Worth doing early because presence, live dashboards, and collaborative cursors are the use cases the
architecture is uniquely suited to, and they are already named in the
[use cases](/guide/use-cases) page.

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


### 12. Computed keys — the reactive cache

Declare a key as a function of other keys; the server recomputes on change and the diff flows through live queries — cache becomes spreadsheet. `cart:42:total` recomputes when any `cart:42:item:*` changes, and every subscribed UI updates. Uses WASM scripting (#7) as the function runtime. Biggest lift, biggest ceiling.

Under consideration behind these: a CRDT text type for collaborative editing (likely embedding an existing Rust CRDT rather than building one), and per-key undo/history on top of the existing op-log machinery.

---

## Hardening & enhancements

Improvements to shipped functionality rather than new surface. Unnumbered because they are small and
independent — pick them off in any order.

### Reliability

**Reconnect backoff jitter.** `on_close()` computes `500ms × 2^attempts` with no randomisation, so
every client disconnected by a server restart reconnects on an identical schedule. At scale that is a
thundering herd that can keep knocking the server over as it comes back up. Full or decorrelated
jitter is a few lines, and the existing backoff test only needs to assert a range instead of an exact
value.

**Exactly-once across a server restart.** `DEDUP` high-water marks live in server memory and are
swept after 24 h idle, so a restart inside the acknowledgement window can admit one duplicate — a
caveat currently documented rather than fixed. Persisting the marks alongside the snapshot would make
the exactly-once guarantee unconditional.

**Atomic WAL compaction.** Browser-side compaction clears the write-ahead log and then writes the
replacement snapshot. An interruption between the two loses the persisted cache. Writing to a new key
range and swapping removes the window entirely.

**Surface outbox overflow.** The client outbox holds 10 000 writes and silently evicts the oldest
past that. An `onOutboxFull` callback, or a `pendingWrites()` accessor, lets an application degrade
deliberately instead of losing writes without a signal.

### Live queries

**Live queries carry collection values.** Collection types currently arrive as *type markers*, so a
subscriber must follow every change with a typed re-read (`HGETALL`, `LRANGE`). That round-trip is
precisely what the local-read model exists to remove, so the promise only fully holds for strings
today. Sending the value — or a delta — inline closes the gap.

**`FLUSHDB` should emit per-key diffs.** Subscribers currently miss a mass deletion entirely.

### Performance

Both of these are diagnosed on the [benchmarks](/guide/benchmarks) page; the analysis is done, the
work is not.

**Byte-slice command arguments.** RESP parsing allocates a `String` per argument. This is the main
remaining lever on unpipelined latency and the likeliest explanation for `HSET` sitting at ~46 % of
Redis while *beating* it pipelined.

**Serialize `LRANGE` straight from the store**, instead of building the full reply `Value` first.

### Operability

**Capacity and sync metrics.** Six series are exported today, all traffic. Nothing reports memory
use, key count, eviction rate, replication lag, or outbox depth — so an operator cannot answer "am I
near the cap?" or "is my replica behind?" from a dashboard. See
[Operations](/server/operations#what-is-not-exported-yet).

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
