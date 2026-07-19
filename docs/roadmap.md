# Roadmap

Recached competes on **where the data can live** — the same engine on the server and in the browser, with sync in between. The [benchmarks](/guide/benchmarks) show this costs nothing in raw speed.

## Near-term

1. **[Byte-transparent values](#byte-transparent-values)** — values are stored as UTF-8 strings, so
   raw binary is corrupted on every transport. The largest remaining drop-in gap.

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

### Performance

**Serialize `LRANGE` straight from the store**, instead of building the full reply `Value` first.
Diagnosed on the [benchmarks](/guide/benchmarks) page.

### Byte-transparent values

**Store values as bytes rather than `String`.** `EntryValue::Str` is a `String`, so a value is forced
through a lossy UTF-8 conversion when the command is parsed: `SET k <0xFF 0xFE>` stores two U+FFFD
replacement characters instead. This is a property of the engine, so it applies to **every**
transport — TCP included, not just WebSocket as previously recorded here.

The practical effect is that compressed blobs, protobuf, images, and anything else Redis users
routinely cache cannot be stored without base64-encoding first. It is the largest remaining gap in
"any Redis client works today".

Closing it means `Vec<u8>`/`Bytes` values through `cmd.rs`, `store.rs`, and the collection types; a
snapshot format change; and an API decision for the browser SDK, whose `set(key, value)` takes a
string. That is a breaking change and wants its own release rather than being folded into a patch.
Current behaviour is pinned by `core-engine/tests/binary_values.rs`, which fails when it changes.

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

- **Command coverage** — closing gaps in the supported command set as real workloads surface them (see [Commands](/server/commands)).

---

Feedback on priorities is welcome — [open an issue](https://github.com/thinkgrid-labs/recached/issues) or write to [dennis@thinkgrid.dev](mailto:dennis@thinkgrid.dev).
