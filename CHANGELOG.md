# Changelog

All notable changes to Recached are documented here.

---

## [0.3.2] — 2026-08-07

### Fixed — `cache.incr()` and `cache.decr()` threw on every call

`incr_by` takes an `i64` in Rust, so wasm-bindgen marshals it as a **`bigint`** —
`incr_by(key: string, delta: bigint): bigint`. The SDK's hand-written `RawCache`
interface declared `number` and `sdk.js` passed one, so every call died with:

```
TypeError: Cannot convert 1 to a BigInt
```

Runtime-independent — the same failure in the browser as in Node. The SDK now converts at the
boundary (`BigInt` in, `Number` out); counters stay 64-bit on the wire and the returned `number`
is exact to `Number.MAX_SAFE_INTEGER`.

The bug dates back to at least 0.2.4 but was **unreachable until 0.3.1**, because no release from
0.1.3 to 0.3.0 could be imported at all (see below). Fixing the packaging is what exposed it.
`incr`/`decr` are the PN-counter primitives the offline-merge documentation is built on, so this
was the worst possible method to have broken.

Why nothing caught it, and what now does:

- **`createCache` casts the module through `unknown`** (`as unknown as RawCache`), which switches
  off type checking at exactly the boundary that drifted. New **`types-check.ts`** asserts
  `RecachedCache extends RawCache` against the real generated `.d.ts`, run by
  `npm run typecheck:bindings` in both CI and the release. Reverting the fix now fails compilation
  naming `incr_by` — verified.
- **No test ever called the SDK.** The `wasm-pack` browser tests are Rust-side and never cross the
  JS boundary; `sdk.js` had no tests. `verify-package.mjs` now `initSync`s the packaged wasm and
  exercises the public `Cache` API — `get`/`set`, TTL, `incr`/`decr`, JSON documents, `getMatching`,
  bytes, `del` — against the extracted **tarball**, not the working tree. Reintroducing the bug
  fails it with the exact `TypeError` — verified.

### Fixed — `useKeyJSON` and `useKeyBytes` crashed the component tree

Both hooks read through `useSyncExternalStore`, which compares snapshots with `Object.is` and
re-reads on every render. `getJSON` parses a **fresh object** and `getBytes` copies a **fresh
array** out of wasm on every call, so each read looked like a change, which forced another render,
which read again. React caught the loop and threw:

```
Maximum update depth exceeded.
```

That happened on mount for **any key that existed** — the hooks worked only while their key was
missing. `useKeys` already guarded against exactly this (it memoises on content), and the guard
simply had not been applied to the single-key hooks.

Both now hold their last snapshot and re-read only when a mutation has actually occurred, tracked
by a counter incremented in the store subscription. `useKeyJSON` additionally compares the stored
string before re-parsing and `useKeyBytes` compares bytes, so a write to an *unrelated* key no
longer changes either hook's identity — which matters for `useKeyBytes`, where a new buffer means
every downstream `URL.createObjectURL` is rebuilt.

Found by the new test suite below, on the first run.

### Added — TypeScript test suites (71 tests)

The TS side had no tests at all. Three suites now run in CI on every push:

- **`recached-edge` (36 tests)** — the SDK wrapper against a fake `RawCache`: null mapping, JSON
  handling and parse failures, `setJSON` TTL routing, `del`'s boolean, the bigint marshalling above,
  `jset`/`jmerge` error propagation, mutation and per-channel pub/sub listener bookkeeping,
  live-query ref-counting (shared patterns, repeated stops, re-subscription), and the order
  `createCache` applies persistence → broadcast → connect → auth → token/scopes → reconnect.
- **`@recached/react` (19 tests)** — provider lifecycle (including that children do not render until
  `createCache` resolves, the reason wrapping a whole app opts it out of SSR), re-render on
  mutation, snapshot identity, live-query start/stop across pattern changes, and pub/sub handler
  identity not forcing a resubscribe.
- **`@recached/vue` (16 tests)** — ref updates, `onUnmounted` releasing every subscription, plugin
  provide/inject and its error when uninstalled.

The fakes are deliberately faithful where it matters: `getBytes` allocates a new array per call, as
the real binding does. A fake returning one shared instance is what would have hidden the crash
above.

### Fixed — packaging

- `@recached/react` and `@recached/vue` shipped without `NOTICE`. The release copies it into both
  package directories, but their `files` array was `["dist/"]`, so it was excluded — `LICENSE.md`
  only survived because npm includes it automatically. Apache-2.0 §4(d) expects the NOTICE to
  propagate; both now list it explicitly.

### Note on 0.3.1

0.3.1 fixed the packaging correctly — `snippets/` ships, `createCache` is exported, the tarball
installs — and that is what made this `incr` bug reachable for the first time. If you installed
0.3.1 and use counters, upgrade.

---

## [0.3.1] — 2026-08-06

A packaging-only release. No engine, server or SDK behaviour changed — but every
`recached-edge` version before this one was impossible to install, so in practice this is the
first usable browser release.

### Fixed — the npm package could not be imported (0.1.3 – 0.3.0)

- **`snippets/` was never published.** The wasm-bindgen glue opens with
  `import { openRecachedDb, … } from './snippets/<crate-hash>/inline0.js'` — the IndexedDB
  helpers behind `enable_persistence`. That directory was in neither the tarball nor the `files`
  array wasm-pack generates, so `npm install recached-edge` followed by any import died with
  `ERR_MODULE_NOT_FOUND` before a line of application code ran. It bundles statically, so
  webpack, Turbopack and Vite all failed at build time, not at runtime. Fourteen consecutive
  releases shipped this way: the break dates from 0.1.3, when the IndexedDB helpers were added.

- **The documented API was not the published one.** The release workflow published
  `wasm-edge/pkg` — raw wasm-pack output, whose only exports are `RecachedCache`, `initSync` and
  a default init. `createCache`, `Cache`, `onMutation`, `getJSON` and the ref-counted `liveQuery`
  live in `sdk.js`, which was never published. Every example in the README and docs imported a
  symbol that did not exist on npm, and both framework SDKs peer-depend on it. The release now
  publishes the `wasm-edge` package itself, with `pkg/` nested inside it.

- **`sdk.ts` imported a filename no build produced.** It loads `./pkg/recached_edge.js`, but the
  crate is named `wasm-edge` (default output `wasm_edge.js`) and the release built with
  `--out-name recached-edge` (output `recached-edge.js`). Both build paths now pass
  `--out-name recached_edge`, matching the import and the stub CI already generated.

- **`pkg/.gitignore` would have silently emptied the fixed package.** wasm-pack writes one
  containing `*`, and npm applies a nested `.gitignore` even to a directory listed in `files` —
  so simply switching the publish directory would have shipped an SDK with no WebAssembly in it.
  `npm run build:wasm` deletes it, and the release deletes it again before packing.

### Added — a release gate that would have caught all of the above

- **`wasm-edge/scripts/verify-package.mjs`**, run by a new `npm-package` CI job on every push and
  by the release workflow immediately before `npm publish`. It packs a real tarball, extracts it
  outside the working tree, walks the import graph from `sdk.js` through the glue to `snippets/`,
  and imports the result in Node to assert `createCache`, `Cache` and `init` are exported. It
  also runs from `prepack`, so a manual `npm publish` cannot bypass it.

  The gap this closes: typecheck, unit tests and `wasm-pack test` all pass against a working tree
  where `snippets/` is present on disk. None of them can observe what `files` excludes. Only
  packing and importing from outside the tree can, and nothing did that.

- `LICENSE.md` and `NOTICE` now ship inside the npm package (`files`), and the meaningless
  `licenseFile: "../LICENSE.md"` key — which pointed outside the package — was dropped.

### Documentation — running the client with no server

Local-only mode was supported in code and mentioned in passing, but never documented as a mode with
edges. It now is, because "can I use this as a client cache without running the server?" has a
sharper answer than the docs were giving:

- **New [use case: no server at all](docs/guide/use-cases.md)** — what works standalone, and a table
  of what is *inert* rather than broken. `publish`/`subscribe`/`onMessage`, `liveQuery`,
  `syncToken`/`syncScopes` and `pendingWrites`/`onOutboxFull` do not throw without a connection; they
  silently do nothing, and pub/sub in particular does **not** fall back to BroadcastChannel. Also
  states where local-only Recached is the wrong choice: against React Query or SWR for a plain
  request cache, a ~550 KB `.wasm` buys Redis semantics and nothing else.
- **The "one-sentence test"** framed Recached as pointless unless a client reads backend-written
  data. It now names the third answer — no server in the picture at all.
- **[Getting Started (Browser)](docs/browser/getting-started.md)** gained the same works/inert split
  next to its local-only example.
- **Known wart, now written down:** with `persistence: true` and no `connect`, every write still
  records an IndexedDB outbox row for a replay that cannot happen, and warns `offline write queue
  full` past 10,000 writes. Nothing is lost; it is wasted I/O and a misleading message.
  (`wasm-edge/src/lib.rs` — `queue_write` skips the outbox only when there is neither a URL *nor*
  persistence.)
- **The Next.js (App Router) example was a no-op** — a provider that returned its children untouched
  and imported a symbol it never used. Replaced with the real client-only pattern: `RecachedProvider`
  in a `'use client'` boundary, plus the plain `useEffect` version. Documents that the provider
  renders `null` until `createCache()` resolves in an effect, so wrapping the whole app opts the page
  out of SSR.

### Changed

- `@recached/react` and `@recached/vue` raise their `recached-edge` peer floor from `>=0.1.4` to
  `>=0.3.1`. The old range was satisfiable only by versions that cannot be imported.
- `wasm-edge/tsconfig.json` adds `ESNext.Disposable` to `lib`: wasm-bindgen 0.2.120 emits
  `[Symbol.dispose]()` on the generated class, which ES2020's lib does not declare.

---

## [0.3.0] — 2026-08-03

### Added

- **`MEMORY USAGE key [SAMPLES count]`.** Recached enforces `maxmemory` and evicts against
  it, but an operator who hit the limit had no command that answered "which key is eating
  it?" — `INFO memory` gives one total and nothing per key. The reply is the same figure the
  eviction loop bills the key for, computed by the same function, so "what is this costing me"
  and "what gets evicted next" cannot drift apart the way two separate estimators would.
  A missing or expired key reports nil, not zero: "no such key" and "an empty key" are
  different facts. `SAMPLES` is parsed and its count discarded — Redis uses it to bound how
  much of a nested value it walks before extrapolating, and Recached always walks all of it,
  so the reply is never less accurate than what was asked for. `MEMORY DOCTOR`, `STATS`,
  `PURGE` and `MALLOC-STATS` are refused with a reason: they describe an allocator arena that
  Recached does not manage and cannot honestly report on.

- **`PUBSUB CHANNELS [pattern]`, `PUBSUB NUMSUB [channel ...]`, `PUBSUB NUMPAT`.** Pub/sub has
  shipped since the first release with no way to see any of it — `PUBLISH` returned a delivery
  count and that was the only observable, so "is anything actually subscribed?" could only be
  answered by publishing and watching the number. The subscriber hub already held both
  registries; these are a read of state that existed all along. `CHANNELS` lists only channels
  with a live subscriber and drops one the moment its last subscriber leaves, `NUMSUB` reports
  a zero for a channel nobody is on rather than omitting it so the reply can be read by
  position, and `NUMPAT` counts distinct patterns, not subscribers. Verified reply-for-reply
  against redis-server 7.2.5. `PUBSUB` is classified as an admin command, so it is refused on
  scope-limited WebSocket connections: a scoped connection may still `SUBSCRIBE` to any channel
  it can name, but naming one and enumerating everyone else's are different powers, the same
  line `GET` and `KEYS` sit on.

- **`MODULE LIST`.** An empty array, which is the answer a stock `redis-server` with no modules
  gives, and which lets a tool tell "no modules" apart from "cannot ask". `MODULE LOAD`,
  `LOADEX` and `UNLOAD` are refused rather than answered `+OK`.

- **`INFO` now reports the `# Cluster` section** — `cluster_enabled:0`, in the default set, so a
  bare `INFO` carries it. This is how a cluster-aware client actually learns it is talking to a
  single node, and Recached had no way to say so.

### Changed

- **`CLUSTER` is now refused with Redis's own sentence** — `ERR This instance has cluster support
  disabled` — instead of `ERR unknown command`. Measuring beat assuming here: a `redis-server`
  not started in cluster mode does **not** answer `CLUSTER INFO` with `cluster_enabled:0`, it
  rejects the whole container with that error and publishes the flag through `INFO`. So the fix
  was not to implement `CLUSTER INFO` — implementing it would have made Recached *less* like
  Redis — but to copy the refusal and add the `INFO` section. `unknown command` was the one
  reply a client cannot act on, because it reads as "too old to ask" rather than "not a cluster".

- **`PUBSUB SHARDCHANNELS` and `SHARDNUMSUB` are refused**, where a standalone Redis answers both
  with an empty array. The one deliberate divergence in this batch: Redis's empty array sits next
  to a working `SSUBSCRIBE`, and Recached implements neither `SSUBSCRIBE` nor `SPUBLISH`, so the
  same reply would invite a follow-up call that fails.

### Added

- **`RECACHED_PORT` and `RECACHED_WS_PORT`.** The RESP and WebSocket ports were compiled in, so two
  instances could not share a host — a replica beside its primary was impossible — and there was no
  way off 6379, the first port any commodity scanner probes. Defaults are unchanged, so no existing
  deployment needs to do anything. The port is not itself a security control (`RECACHED_BIND`, the
  password, TLS and the allowlists are), which is why exposing it is safe; what is *not* safe is a
  typo falling back to the default, so an unparseable value, `0`, or the two ports being equal is a
  startup error rather than a silent 6379. Ports below 1024 still require root, enforced by the OS.
  The startup warnings about the sync port now name the port actually in use instead of a hardcoded
  6380, and `INFO`'s `tcp_port`/`recached_ws_port` report the real values.

### Fixed

- **`RECACHED_METRICS_PORT=0` now disables the exporter, as the reference has always said it did.**
  `0` parsed fine and `bind("host:0")` hands the listener an OS-assigned ephemeral port, so an
  operator switching metrics *off* got them served on an unpredictable port instead — the opposite
  of the request, and unlikely to be noticed until something scraped it. A collision on this port
  also aborted startup with a bare `panic!` and a backtrace note, which reads like a bug in Recached
  rather than two servers wanting one port; it is now an error that names the conflict and points at
  the three port variables. An unparseable value is a startup error.

- **`INCR` and friends no longer clear a key's TTL on the replica, in the AOF, or in synced
  browsers.** Counters propagate by value — `SET key <new value>` — so that a replica which missed a
  frame converges on the primary's number instead of compounding its own. But a bare `SET` also
  clears the expiry, and Redis's `INCR` leaves it untouched, so the single most common expiring
  counter idiom — `INCR key` followed by `EXPIRE key window` — replayed as a key with no expiry at
  all. The rate-limit bucket, the per-minute quota and the retry counter each became permanent
  everywhere but on the primary, and the window never reset because the key it keyed on never went
  away. They now propagate as `SET <value> KEEPTTL`, which keeps the by-value convergence while
  leaving the deadline where the primary has it. `GETSET` still propagates a bare `SET`, because
  `GETSET` really does clear the TTL.

- **`TTL` rounds to the nearest second instead of truncating.** `SET k v EX 100` followed immediately
  by `TTL k` answered 99: the microseconds spent between the two commands took the remainder just
  below 100 000 ms, and integer division discarded the rest. Every reading was up to a second short,
  which breaks a ported test suite asserting the value it just set and makes any client that renews
  below a threshold renew early on every pass. Now `(remaining_ms + 500) / 1000`, matching Redis.
  `PTTL` is unchanged — it reports milliseconds and has nothing to round.

- **`PUBLISH` may be used inside `MULTI`.** Redis allows it, and announcing a change atomically with
  the write that caused it is an ordinary reason to open a transaction at all, but Recached refused
  it alongside `SUBSCRIBE` and `WATCH`. Simply letting it queue would have been worse than the
  refusal: delivery lives in the connection loop and the store's `PUBLISH` is a stub that answers 0
  and sends nothing, so the message would have been swallowed silently and `EXEC` would have reported
  a plausible zero. `EXEC` now dispatches queued publishes to the subscriber hub itself, so the reply
  is the real delivery count and subscribers actually receive the message. `SUBSCRIBE`, `PSUBSCRIBE`
  and `WATCH` remain unqueueable, which is correct.

- **The binary is `recached-server` and the crate is `recached`, as the docs have always claimed.**
  The package was `server-native` and produced a `server-native` binary, so `cargo install recached`
  installed nothing — the name was not even registered — and `cargo build --bin recached-server`,
  the command in the contributing guide, failed outright. Only Docker and Homebrew worked, because
  both rename the artefact as they copy it. The directory keeps its name; it describes the role,
  while the package describes the product. CI, the release workflow and the Dockerfile follow.

- **Expiries now propagate as absolute deadlines, so a restart no longer resurrects a key that
  should have died during it.** Every write leaves the server as one RESP frame that the AOF, the
  replication log and the browser sync fan-out all consume, and a relative TTL (`SET k v PX 5000`,
  `PEXPIRE k 5000`) was written into that frame verbatim. Each consumer then re-based the deadline
  onto *its own* clock at *its own* arrival time, so the key's lifetime silently restarted on every
  hop. At AOF replay this was total: a key written with `EX 5` and replayed an hour later came back
  alive with a fresh five seconds — a revoked session, an abandoned lock, a spent idempotency key or
  a rate-limit window, all restored by the restart that was supposed to be transparent. Replicas
  expired their copy later than the primary by the replication delay, and a reconnecting browser
  reset the TTL of every key the sync socket replayed to it. Relative expiries are now converted
  against the propagation timestamp and travel as `PXAT`/`PEXPIREAT`, which is what Redis does and
  what the already-correct `EXAT`/`PXAT`/`EXPIREAT`/`PEXPIREAT` arms beside them already did. An
  absolute deadline is idempotent under replay: applying it once or a thousand times, now or after
  an hour of downtime, names the same instant, and one already in the past needs no special case —
  the store reads such an entry as expired and the sweeper reaps it. The snapshot path was never
  affected; it has always stored absolute expiries, which is why `SAVE` and the AOF disagreed about
  whether a key still existed.

- **`EXEC` no longer runs the rest of a transaction after a command failed to queue.** Redis
  refuses an unrecognised verb at queue time and poisons the transaction, so `EXEC` replies
  `EXECABORT Transaction discarded because of previous errors.` and runs nothing. Recached parses an
  unrecognised verb into an internal `Unknown` command, which queued happily behind a `+QUEUED` and
  only errored while executing — leaving every *other* command in the transaction applied. On a
  server that implements a deliberate subset of Redis that is a live hazard rather than a corner
  case: `MULTI; ZPOPMIN q; LPUSH processing x; EXEC` pushed onto `processing` without ever popping
  `q`, silently, and MULTI is exactly the construct a caller reaches for to prevent that. Anything
  that fails to queue now sets the abort flag — an unknown verb, a frame that will not parse (bad
  arity, malformed argument), a command not allowed inside a transaction, and a queue over
  `RECACHED_MAX_MULTI_QUEUE` — and the unknown-verb rejection is delivered at queue time with the
  same wording the store gives outside a transaction. `MULTI` and `DISCARD` clear the flag, so a
  poisoned transaction does not wedge later ones on the same connection. The CAS abort is
  deliberately left distinct: a `WATCH` conflict still replies with a nil array, because "retry me"
  and "fix your request" are different answers and a retry loop must be able to tell them apart.
  Fixed on both the TCP and WebSocket command paths.

- **The command reference no longer claims Recached exports latency histograms.** `INFO`'s "not
  implemented" note said per-command latency was on the Prometheus endpoint instead — it is not,
  and never was. Recached has no latency instrumentation at all: `recached_commands_total` counts
  calls and `recached_command_errors_total` counts failures, and neither says how long anything
  took. The docs now say so plainly and point at the bounded reads (`HSCAN`, `SSCAN`, `ZSCAN`,
  `GETRANGE`) as the way to avoid the slow paths, since there is no way to catch them after the
  fact. `SLOWLOG` and latency histograms remain unimplemented.

---

## [0.2.4] — 2026-08-02

### Added

- **`QUIT`.** Every client library closes a connection by sending `QUIT` and reading `+OK`.
  Recached answered `ERR unknown command`, and node-redis surfaced that as a thrown error from
  `.quit()` — a clean shutdown reported as a failure. It now replies `+OK` and closes. Answered
  before the authentication gate and before the subscribe-mode gate, as Redis does: a client that
  cannot authenticate, or is parked in subscribe mode, still deserves a clean close rather than a
  dropped socket. The subscribe-mode error already claimed `QUIT` was allowed there; now it is.

- **`CLIENT ID | INFO | LIST | GETNAME | SETNAME | SETINFO`.** node-redis, ioredis, redis-py and
  go-redis all send `CLIENT SETINFO LIB-NAME` and `LIB-VER` immediately after `HELLO`. All four
  tolerate the error, so nothing broke — but every connection from every modern client logged two
  or three `unknown command` errors before doing any work, and the server could not say which
  library was on the other end of a connection. `CLIENT LIST` and `CLIENT INFO` report in Redis's
  `key=value` line format, carrying the fields Recached can answer truthfully — id, addresses,
  name, age, subscription counts, protocol, library — and omitting the buffer sizes, file
  descriptors and event masks it cannot. A plausible invented `omem` is worse than an absent one,
  because nothing downstream can tell the difference. `CLIENT KILL`, `NO-EVICT` and `UNPAUSE`
  return "unknown subcommand" rather than `+OK`: answering OK without doing the thing would leave
  a caller believing a connection had been closed or eviction disabled.

- **`CONFIG GET`.** Reports `maxmemory`, `maxmemory-policy`, `maxclients`, `port`, `tls-port`,
  `appendonly`, `databases`, `requirepass`, `proto-max-bulk-len`, `timeout` and `save`, resolved
  from the values actually in force rather than a table of defaults — the eviction policy comes
  from the store, the ports and limits from the same startup facts `INFO` reads. Glob patterns and
  multiple names work as in Redis. `requirepass` is masked to `*`: whether a password exists is not
  a secret, its value is. **`CONFIG SET` is refused with an explanatory error.** Recached reads its
  configuration from the environment at startup and holds it for the life of the process, so there
  is nothing a runtime SET could change; returning `+OK` would leave an operator to discover much
  later that the limit they set never applied.

- **`COMMAND`, `COMMAND COUNT`, `COMMAND LIST`, `COMMAND INFO`, `COMMAND DOCS`.** Backed by a new
  `core-engine::catalog` module holding one row per command. Arity, flags and key positions are
  transcribed from a real `redis-server` 7.2.5's own `COMMAND INFO` rather than written from
  memory: cluster-aware clients and proxies route on `first_key`/`step`, and a wrong arity makes a
  client reject a call the server would have accepted. Summaries come from
  `docs/server/commands.md`, so the text a client sees is the text that was reviewed and published.
  The catalog cannot drift from the parser: one test asserts every catalog row names a command the
  parser accepts, and another walks the exhaustive `Command` variant table asserting every command
  has a row.

  Worth recording, since it contradicts a guess made while planning this work: **no client library
  sends `COMMAND DOCS` or `CONFIG GET` during connect.** Tapping the wire for node-redis 6.2.0,
  ioredis 6.0.0, redis-py 8.1.0 and go-redis 9.21.0 showed all four opening with `HELLO 3`,
  `CLIENT SETINFO` ×2 and `CLIENT MAINT_NOTIFICATIONS`, and nothing else. `COMMAND DOCS` is used
  interactively by `redis-cli`, and `CONFIG GET maxmemory-policy` by background-job frameworks —
  both worth having, neither on the connect path.

`CLIENT MAINT_NOTIFICATIONS` is still refused. It asks the server to push notifications before a
maintenance event moves the connection elsewhere; Recached has no such event to announce, and every
client that sends it treats the error as "not supported here" and carries on.

- **Bounded reads: `GETRANGE`, `HSCAN`, `SSCAN`, `ZSCAN`.** Until now the only way to read a hash,
  a set or a sorted set was `HGETALL` / `SMEMBERS` / `ZRANGE`, and the only way to read a string was
  `GET`. Each of those is unbounded: the reply is as large as the value, and building it clones the
  whole collection while holding the shard guard for that key — so one oversized key delays every
  other key that hashes to the same shard. The cursor variants bound both the reply and the time
  the guard is held, and `GETRANGE` reads a byte window of a large value without transferring it
  whole. Tools that browse a keyspace they did not write — `redis-cli --scan`, TUI browsers such as
  keylens, admin dashboards — need exactly these four to stay bounded; without them they must
  measure with `STRLEN` / `HLEN` / `SCARD` and refuse to display anything big.

  ```bash
  GETRANGE log:2026-08 0 4095            # first 4 KB of a large value
  HSCAN session:42 0 COUNT 100           # → [cursor, [field, value, …]]
  HSCAN session:42 0 NOVALUES            # field names only (Redis 7.4)
  SSCAN tags:hot 0 MATCH "eu:*"
  ZSCAN leaderboard 0 COUNT 50           # → [cursor, [member, score, …]]
  ```

  The cursor is an offset into the collection's name-ordered element list, the same scheme keyspace
  `SCAN` already used, so the same caveat applies: elements added or removed mid-iteration may be
  missed or returned twice, and `MATCH` must stay the same across an iteration. `ZSCAN` orders by
  **member**, not by score — a score can change under an in-flight cursor, and only the member
  ordering survives that. A cursor pointing past the end of a collection that shrank returns an
  empty final page rather than an error.

### Security

::: danger Breaking: the replication port no longer opens by default
If you run replication, add `RECACHED_REPL_ENABLE=1` to every node that serves replicas —
including a replica that serves sub-replicas. Without it, port 6381 is not bound and replicas
cannot attach.
:::

- **The replication port is now opt-in, and refuses to run unauthenticated on a public interface.**
  It previously bound `${RECACHED_BIND}:6381` — default `0.0.0.0` — unconditionally on **every**
  node, whether or not replication was configured. With `RECACHED_REPL_PASSWORD` unset, which was
  also the default, the handshake was skipped entirely and any peer that connected received a full
  MessagePack dump of the keyspace followed by a live stream of every subsequent write. An operator
  who set `RECACHED_PASSWORD` had every reason to believe the data was behind authentication, and
  it was not: the port bypassed it completely.

  Two rules now apply. The listener binds only when `RECACHED_REPL_ENABLE` is set, and enabling it
  on any interface other than loopback without `RECACHED_REPL_PASSWORD` **refuses to start** rather
  than serving the keyspace unauthenticated. Multi-tier replication is unaffected in capability —
  a node that serves sub-replicas sets the variable — but it is now a decision rather than a
  default. All earlier 0.1.x and 0.2.x releases are affected; upgrading is the fix.

  The same listener now also honours `RECACHED_ALLOW_IPS` and counts against
  `RECACHED_MAX_CONNECTIONS`. Neither applied to it before, so the one port that streams the entire
  keyspace was the one port with no allowlist and no connection limit.

- **Failed replication auth is throttled per source address.** The RESP port drops a connection
  after five wrong passwords, but the replication handshake is one-shot: a wrong guess cost an
  attacker a single TCP connection, so reconnecting gave unlimited attempts at a secret that yields
  the whole keyspace. Failures are now counted per peer over a rolling window and further attempts
  are refused before the handshake is read.

- **The replication handshake no longer leaks the password length.** It read exactly
  `password.len() + 1` bytes, so the number of bytes the server waited for *was* the length —
  recoverable by feeding one byte at a time and watching when the server replied. It now reads to
  the line terminator, with a length cap and a deadline.

- **`RECACHED_ALLOWED_ORIGINS` restricts which web pages may open the browser sync socket.**
  Browsers apply neither CORS nor a preflight to WebSockets, so any page a user visited could open
  a socket to a reachable Recached and read or write the keyspace with that user's network
  position — on the common `ws://localhost:6380` development setup, every site in every tab. Set it
  to a comma-separated list of exact origins (`https://app.example.com,http://localhost:3000`;
  `null` admits sandboxed iframes and `file://` documents) and a handshake from anywhere else is
  refused with a 403.

  Unset means allow-all with a startup warning, matching how `RECACHED_PASSWORD` behaves — the
  project ships insecure by default and says so, but it should not do it silently. A client that
  sends no `Origin` header at all is admitted: native clients omit it and an attacker with a raw
  socket can forge it, so refusing would break real clients while stopping nobody. The control
  separates *the application you deployed* from *another page in the same browser*, which is the
  threat this port actually faces.

- **TLS and WebSocket handshakes are now bounded by a deadline** (`RECACHED_HANDSHAKE_TIMEOUT`,
  default 10s). The connection permit is taken before the handshake runs, so a client that opened a
  socket and then said nothing held one of `RECACHED_MAX_CONNECTIONS` slots indefinitely. A
  thousand such sockets cost an attacker nothing and stopped the server accepting real clients.

- **Snapshots, the AOF and the dedup sidecar are created `0600`.** They were created with the
  process umask — `0644` on a typical host — so any local user could read plaintext MessagePack
  dumps of the entire keyspace. Files left `0644` by an earlier version are tightened on the next
  write. Snapshot and dedup temp files also carry the process id, so two servers sharing a data
  directory can no longer clobber each other's half-written file.

- **The RESP parser no longer reserves memory for a count it has not received.** An aggregate header
  declares how many elements follow and arrives before any of them, and the parser reserved capacity
  for the declared count. Nine bytes — `*1000000\r\n` — reserved 32 MB, and nesting that to the depth
  limit made 160 bytes of input request **512 MB**. Because an incomplete frame is re-parsed from the
  start whenever more bytes arrive, a client dripping one byte per packet repeated the reservation on
  every packet, and none of it counted against `RECACHED_MAX_MEMORY`, which tracks stored data only.

  Reservations are now capped at 1,024 elements, so allocation is proportional to bytes *received*
  rather than bytes *claimed*: the same two inputs now cost 32 KB and 512 KB. The cap is a floor and
  not a limit — aggregates larger than 1,024 elements still parse in full, and `proto-max-bulk-len`
  and the million-element ceiling are unchanged.

- **Replication can now be encrypted and the primary's identity verified.** Both ends of port 6381
  were plain TCP, so the replication password and then the entire keyspace crossed the network in the
  clear. Worse than the eavesdropping: a replica had no way to check *who* it was following, so a DNS
  hijack or an on-path attacker could feed it an arbitrary keyspace and it would load it.

  The listener now reuses `RECACHED_TLS_CERT`/`RECACHED_TLS_KEY` — enabling TLS covers the RESP,
  WebSocket **and** replication listeners. Replicas opt in with `RECACHED_REPL_TLS_CA`, pointing at
  the primary's certificate or the CA that issued it, with `RECACHED_REPL_TLS_SERVERNAME` to override
  the verified name when `RECACHED_REPLICAOF` names an IP but the certificate names a host.

  The trust anchor is a file rather than the system root store deliberately: replication links two
  hosts one operator runs, so trusting one private CA is both simpler and tighter than trusting every
  public CA to vouch for a host that streams the whole dataset. A public bundle still works if the
  primary's certificate is publicly issued. There is no encrypt-without-verify mode, because
  verification is the half that stops a rogue primary.

  Note that this needs a genuine **two-certificate chain** — a CA plus a leaf it signed. A single
  self-signed certificate, which is what the common `openssl req -x509` one-liner produces, is marked
  `CA:TRUE` and is refused as a server certificate (`CaUsedAsEndEntity`). OpenSSL-based clients like
  `redis-cli --cacert` accept such a certificate, so the same file can work for `rediss://` and fail
  for replication. The security docs carry the `openssl` recipe.

  Replication remains **plaintext unless configured**, and a replica following a primary without TLS
  now says so at startup. A replica without `RECACHED_REPL_TLS_CA` cannot talk to a TLS-enabled
  primary; that handshake failure names the variable to set.

- **Glob patterns are capped at 1,024 bytes** for `KEYS`, `SCAN`/`HSCAN`/`SSCAN`/`ZSCAN MATCH`,
  `QSUB`, `PSUBSCRIBE`, `SYNC` scopes and signed sync tokens, reported as
  `ERR pattern is too long`. Matching costs O(pattern × text) and runs once per key for `KEYS`, once
  per key per write for sync scopes, and once per published message per subscriber for `PSUBSCRIBE`;
  pattern length was previously bounded only by `proto-max-bulk-len` at 64 MB, so a single command
  could occupy the process for an unbounded time. No legitimate pattern is near the cap.

### Fixed

- **`RECACHED_AOF_SYNC` now actually fsyncs.** `always` and `everysec` called `flush()`, which pushes
  the buffer into a `write` syscall and leaves the bytes in the page cache — so acknowledged writes
  survived a process crash but *not* a power loss or kernel panic, which is the case `always` exists
  to cover. Both now `fsync`. The AOF truncation that follows a snapshot is fsynced too, so a crash
  cannot resurrect a log the snapshot has already subsumed.

  Snapshots gained the same treatment: the temp file is fsynced before the rename, and the containing
  **directory** is fsynced after it. Without the second step the file contents were durable but the
  directory entry naming them was not, so a crash could leave the previous snapshot or none at all.

  ::: danger `always` is now roughly 400× more expensive per write
  It was never doing the work, so it never cost anything. Measured on macOS/APFS: `no` and `everysec`
  both run ~40–50 µs per append, `always` runs **~20 ms** — tens of writes per second rather than tens
  of thousands, because the fsync is held inside the AOF lock and every writer queues behind it. The
  server now logs a warning at startup when `always` is selected.

  If you benchmarked `always` on an earlier version and found it acceptable, re-measure: that number
  was the cost of a `write` syscall, not of durability. `everysec` remains the default and costs
  nothing measurable. See [Configuration](/server/configuration#what-each-sync-mode-costs).
  :::

- **`glob_match` no longer allocates.** It ran an O(pattern × text) dynamic program that allocated two
  `Vec<bool>` of `text.len() + 1` on **every call**, so one 64 MB value made `KEYS *` request 128 MB
  on a path that runs once per key. Replaced with two-pointer greedy matching: same worst-case time
  bound, zero allocation. Semantics are unchanged, verified by differential testing against the
  previous implementation over every pattern of up to 5 bytes drawn from `{a, b, *, ?}` against every
  text of up to 4 bytes drawn from `{a, b}`.

- **Documentation: glob character classes were never supported.** `commands.md`, `sync-scopes.md` and
  the `glob_match` doc comment all described `[abc]` as a character class. It has never been
  implemented — brackets match literally, so `[ab]` matches the four-byte string `[ab]` and not `a`.
  This matters most for sync scopes: a scope written `user:[12]:*` grants access to keys starting with
  the literal text `user:[12]:` and matches nothing a normal application writes. It fails closed, so
  no data was over-exposed, but a scope written that way never granted what its author intended.
  Enumerate prefixes instead — `user:1:*,user:2:*`.

### Changed

- **`SCAN` now rejects `COUNT 0` and negative counts** with `ERR syntax error`, as Redis does.
  They were previously cast to `usize`, where `-1` wrapped to a count of 18 quintillion and was
  then silently clamped back to a normal page. The cursor argument reports `ERR invalid cursor`
  rather than `ERR value is not an integer or out of range`, which is both Redis's wording and
  what the whole SCAN family now shares.

---

## [0.2.3] — 2026-08-01

### Added

- **`INFO [section ...]`.** Recached previously answered `INFO` with `-ERR unknown command`, which
  breaks more than `redis-cli info`: client libraries call it during their connection ready-check,
  and monitoring agents treat its absence as a dead server. It now reports the `server`, `clients`,
  `memory`, `persistence`, `stats`, `replication`, `keyspace`, and `recached` sections in Redis's
  `# Section` / `field:value` format, with CRLF endings, so existing tooling parses it unmodified.
  Section arguments are honoured in the order given; `all`, `everything`, and `default` alias the
  default set; unknown sections return nothing rather than an error, as in Redis.

  `redis_version` reports **`6.2.0`** rather than Recached's own version. Clients feature-gate on
  that field, and one reading `redis_version:0.2.3` concludes the server predates everything and
  disables capabilities it could safely use. 6.2 is the honest floor — RESP3 and `HELLO` exist
  there and Recached implements both — and the real version ships alongside as `recached_version`,
  the same split KeyDB and Dragonfly use. `role` likewise reports `master`/`slave` because tooling
  greps for exactly those strings, with `connected_replicas` as a readable alias.

  `INFO` requires authentication and is refused on scope-limited WebSocket connections: it reports
  server-wide state, which a connection granted a handful of keys has no business reading. The
  `cpu`, `commandstats`, `latencystats`, and `errorstats` sections are deliberately absent —
  per-command counters and latency histograms stay on the Prometheus endpoint, where dashboards and
  alerting can use them properly.

### Changed

- **The 5-second metrics sampler walks the keyspace once instead of twice.** `recached_keys` and
  `recached_memory_bytes` each triggered a full scan; they now share a single pass via the new
  `KeyValueStore::keyspace_sample()`, which also supplies `INFO`. `INFO` reads that cached sample
  rather than rescanning, so polling it once a second costs no more than polling it once a minute —
  `used_memory` and the keyspace counts are at most 5 seconds stale, and both were already
  approximations.

- **Repository moved to [`recached-dev/recached`](https://github.com/recached-dev/recached).** Crate
  metadata, the three npm package manifests, the Homebrew formula, the `ghcr.io` image path, and all
  documentation links now point at the new organisation. The GitHub Actions workflows derive the
  image name from `github.repository` and needed no change.

---

## [0.2.2] — 2026-07-20

### Added

- **`ESET` — connection-scoped keys for presence.** A key written with `ESET` lives exactly as long
  as the connection that wrote it; when that connection closes the server deletes it and pushes the
  deletion to live queries. Presence, cursors and "who is online" previously had to be hand-rolled
  with `SETEX` plus a heartbeat, which leaves ghost entries for the length of the TTL whenever a tab
  closes.

  Ownership transfers on each write, which is what makes multiple tabs behave: two tabs both setting
  `presence:user:42` leave the **later** one as owner, so closing the first does not mark the user
  offline. Replicas receive the write as a plain `SET` — they have no connection to scope a lifetime
  to, and the owning server broadcasts the deletion.

- **`onOutboxFull()` and `pendingWrites()` on the browser SDK.** The offline queue holds 10 000
  writes and evicts the oldest past that — previously in silence, with no error and no signal, so an
  application could not tell that a user's write had been discarded. `onOutboxFull(cb)` reports the
  dropped row id and the remaining depth; `pendingWrites()` exposes the depth for a "syncing…"
  indicator or to apply back-pressure before the cap is reached.

- **Capacity and sync metrics.** Seven new series, sampled every 5 seconds because capacity is a
  level rather than an event: `recached_memory_bytes`, `recached_keys`, `recached_evictions_total`,
  `recached_replicas_connected`, `recached_live_queries`, `recached_watched_keys`, and
  `recached_dedup_clients_tracked`, and `recached_replication_queue_depth`. Previously only traffic
  was exported, so an operator could not
  answer "am I near the cap?" or "is eviction thrashing?" from a dashboard. Replication lag landed
  separately in this release; browser outbox depth remains unexported because it lives in the client
  — see [Operations](docs/server/operations.md).

- **`HELLO` and RESP3 negotiation on the TCP port.** A connection starts in RESP2 and `HELLO 3`
  switches it to RESP3; `HELLO 2` switches back, a bare `HELLO` reports without changing, and an
  unsupported version returns `-NOPROTO` while leaving the connection on what it had. The reply is a
  RESP3 map on a RESP3 connection and a flat array on a RESP2 one, matching Redis. `HELLO` requires
  authentication — the pre-auth reply carries no server details, so it cannot be used to fingerprint
  a deployment.

  This also adds a `Map` type (`%N`) to the RESP codec, with the header counting *pairs* rather than
  elements.

- **Replication offset acknowledgement, and a true lag metric.** Replicas now acknowledge each frame
  they apply on the existing replication socket, and the primary exports
  `recached_replication_lag_frames` — frames sent to the furthest-behind replica but not yet
  acknowledged.

  `recached_replication_queue_depth` only ever showed work stuck in the primary's send queue, so the
  case that matters most read as healthy: a replica that has received everything and is not applying
  it shows an empty queue and unbounded lag. Acknowledgements are monotonic, so a reordered or
  replayed ack cannot walk the high-water mark backwards.

  A replica older than 0.2.2 never acknowledges, so its lag climbs while replication works normally —
  upgrade both ends together.

### Changed

- **Command arguments are moved rather than copied.** `extract_string` built every argument with
  `from_utf8_lossy(..).into_owned()`, which allocates a fresh `String` and memcpys the payload even
  when the bytes are already valid UTF-8 — which they nearly always are. Parsing now moves the byte
  buffer the RESP parser already allocated, so a 1 MB `SET` value costs no copy at all.

  Applied to the commands that actually carry payloads: `SET`, `MSET`, `HSET`/`HMSET`, `APPEND`,
  `GETSET`, `SETNX`, `JSET`, `JMERGE`, and all 22 bulk argument lists (`RPUSH`, `SADD`, `ZADD`, …).
  The long tail of small-argument commands still copies; the remaining win there is negligible and
  the risk of touching 125 parse arms is not.

  Argument slots are consumed, so an arm must read each index once — new tests cover the shapes
  where that matters (`SET`'s option scan after moving key and value, `MSET`/`HSET` pair splitting,
  a 1 MB round-trip, and invalid UTF-8 falling back to a lossy re-encode).

  **Not benchmarked.** The gain is a removed allocation and memcpy per argument, which is
  structural, but the machine available could not produce trustworthy figures — see the
  [benchmarks](docs/guide/benchmarks) caveat. Re-measure on a quiet host before quoting numbers.

- **Rate-limiter memory is bounded.** A limiter stored one timestamp per attempt, so
  `RLSET key 100000 3600` held 100 000 `u64`s — roughly 800 KB for a single key, and token-cost
  limiting (roadmap #9) makes six-figure limits ordinary. Attempts are now counted into 64 buckets,
  capping a limiter at about 1 KB whatever the limit.

  The trade-off is granularity: the window advances one bucket at a time, so a limiter is exact to
  within `window / 64`. Attempts are never under-counted — a bucket leaves the window only once it
  is entirely outside it — so the limiter errs toward rejecting slightly early rather than admitting
  over the limit. `retry_after_ms` is clamped to the window.

  Limiter *attempt* state is no longer persisted in snapshots (configuration still is). It ages out
  within a single window and a restart has already interrupted that window, so a restored limiter
  enforces the same policy from a clean slate rather than a stale partial count.

- **Per-connection limits are configurable** rather than compiled in, because the right value is
  workload-dependent: `RECACHED_MAX_MULTI_QUEUE`, `RECACHED_MAX_WATCHES_PER_CONN`,
  `RECACHED_MAX_LIVE_QUERIES`, `RECACHED_MAX_QSUB_INITIAL_KEYS`, and `RECACHED_EVICTION_SAMPLE` (the
  knob Redis exposes as `maxmemory-samples`). Defaults are unchanged. The browser outbox cap is now
  settable through `sync-client` rather than fixed at 10 000.

- **Live queries now carry collection values.** `qstate` and `keychange` previously delivered only a
  *type name* for hashes, lists, sets, sorted sets and JSON, so every subscriber had to follow up with
  `HGETALL`/`LRANGE`/`JGET` — a network round-trip in a system whose premise is that reads are local.
  Collections now arrive **type-tagged and complete**:

  ```text
  hash  →  ["hash", field, value, ...]     fields sorted
  list  →  ["list", element, ...]          head to tail
  set   →  ["set", member, ...]
  zset  →  ["zset", member, score, ...]    ascending score
  json  →  ["json", document]
  ```

  The tag is required for the payload to be unambiguous — a four-element array would otherwise be
  indistinguishable between a list of four items and a hash of two pairs. Ordering is deterministic
  so two clients build identical local state, and each notification carries the complete value, which
  is what allows a removed member to propagate.

  ::: warning Wire-format change
  A client older than 0.2.2 does not understand the tagged shape and will ignore collection values
  from live queries. Server and SDKs are released in lockstep at the same version — run matching
  versions.
  :::

- **`FLUSHDB` now reaches live queries.** `primary_keys()` is empty for `FLUSHDB`, so the generic
  notifier had nothing to announce and subscribers silently kept serving data the server had already
  wiped.

  Announcing per deleted key would mean one frame per key in the keyspace for a single command, so
  the server emits **one sentinel per registered pattern** instead — a `keychange` whose key is the
  pattern and whose value is nil. Clients expand it locally to "every key matching this pattern is
  gone", which is O(patterns) rather than O(keys). Explicitly `WATCH`ed keys are still notified
  individually, since that set is bounded and callers expect per-key precision there.

- **Reconnect backoff is jittered.** Delays now land in `[nominal/2, nominal]` instead of an exact
  `500ms × 2^attempts`. Without jitter every client disconnected by the same event computes an
  identical schedule and reconnects in lockstep — a thundering herd that can keep a recovering server
  down. The jitter source is seeded from the client id rather than a system RNG, so `sync-client`
  stays dependency-free and I/O-free and the sequence remains reproducible in tests.

### Fixed

- **Values were silently corrupted unless they were valid UTF-8; they are now byte-transparent.**
  Values were stored as `String`, so a value containing invalid UTF-8 was converted to U+FFFD
  replacement characters on the way in. `SET` returned `OK`, `GET` returned bytes that differed from
  what was written, and nothing anywhere reported a problem — the original bytes were destroyed at
  parse time, before storage, so there was nothing to recover.

  This affected **every transport, TCP included** — not only WebSocket, as the roadmap previously
  recorded. `SET k <0xFF 0xFE 0x41>` over plain RESP came back as `EF BF BD EF BF BD 41`.

  Values are now stored and returned as the exact bytes sent, matching Redis. The change runs the
  full depth of the stack: the store's string, list and hash types; command parsing; the RESP
  encoder used for replication, AOF and browser sync; pub/sub payloads; and the browser's IndexedDB
  write-ahead log.

  **Identifiers stay text.** Keys, hash fields, set and sorted-set members, glob patterns and channel
  names must be valid UTF-8, and a command carrying a binary one is refused before anything is
  written:

  ```
  ERR argument 1 is not valid UTF-8. Keys, fields, members and patterns must be text;
      only values may be binary
  ```

  Redis permits binary there too, but those positions are looked up, glob-matched and checked against
  sync scopes as text, so a binary identifier would be unreachable through its own access paths. It
  is recorded on the [roadmap](docs/roadmap.md) rather than scheduled.

  **Snapshots remain compatible.** A snapshot written by 0.2.1 or earlier still loads: values were
  msgpack strings then and are msgpack binary now, and the decoder accepts either. Binary values
  encode as msgpack `bin` rather than an array of integers, so snapshot size is unchanged for text
  and roughly halved versus the naive encoding for binary.

  **The browser SDK handles binary end to end.** `setBytes()` / `getBytes()` and `publishBytes()`
  are new, and binary survives the offline outbox, the exactly-once `DEDUP` envelope, cross-tab
  `BroadcastChannel` sync and IndexedDB persistence unchanged. Frames that carry binary now travel
  in WebSocket *binary* frames in both directions — the socket's `binaryType` is `arraybuffer`, so
  an inbound binary frame is no longer silently dropped by the message handler.

  `cache.get()` now **throws** on a binary value instead of returning mangled text, `getJSON()`
  treats one as a miss, and an `onMessage` listener receives a `Uint8Array` for a binary pub/sub
  payload — so the listener signature widened to `string | Uint8Array`. `getMatching()` — the read
  behind every live query — returns a `Uint8Array` for a binary value rather than a lossy string,
  which was the last silent conversion left on the browser read path.

  **`@recached/react` and `@recached/vue`** follow: `useKeyBytes()` is new, `usePubSub` handlers and
  the `KeyValuePair` value type widened to `string | Uint8Array`, and `useKey` returns `null` for a
  binary value rather than letting `get()` throw out of a React `getSnapshot` or a Vue reactive
  update — which would have taken down the render tree over a value the hook cannot represent.

  These are **compile-time breaking changes for TypeScript users** who annotated a `usePubSub`
  handler or an `onMessage` listener as `(msg: string) => void`, or who destructured a
  `KeyValuePair` value as `string | null`. Widen the annotation, or narrow with `typeof v === 'string'`.

  Data corrupted by an earlier version cannot be recovered and must be re-populated.

- **Pub/sub deliveries never reached a TCP subscriber that only listened.** Deliveries were written
  into a 32 KB buffered writer and flushed only at the end of a client-command batch, so a
  connection that subscribed and then waited received nothing until it happened to send another
  command or 32 KB of messages accumulated. A subscriber that also polled looked fine, which is how
  this survived. Deliveries are now flushed on write.

- **Pub/sub frames were RESP3 Push on RESP2 connections.** Every delivery was a `>` frame regardless
  of protocol. RESP2 has no push type, so a standard Redis client that subscribed without sending
  `HELLO 3` could not parse what it was sent. Frame type now follows the negotiated version. The
  WebSocket transport is unchanged — it is RESP3 by definition, and `HELLO 2` on it is refused
  rather than silently ignored.

- **WebSocket command frames must now be accepted in binary as well as text.** The handler matched
  text frames only, so a binary frame was dropped without a reply — and the WebSocket spec requires
  text frames to be well-formed UTF-8, which left no way to send bytes at all. Replies are sent as
  text when the RESP bytes are valid UTF-8 and binary otherwise, so existing clients see no change.

  This makes the transport byte-clean; the values travelling over it became byte-transparent in the
  same release — see below.

- **Exactly-once delivery now survives a server restart.** Dedup high-water marks were held only in
  memory, so a restart inside the acknowledgement window let a client's replayed write apply twice —
  the last standing caveat on the guarantee. Marks are now persisted to a `.dedup` sidecar beside the
  snapshot, written atomically and only when a mark advances, and restored before the server accepts
  connections.

  The map is one `u64` per client, so it is flushed on a 1-second timer as well as with each
  snapshot: the residual window on an unclean shutdown is bounded by that interval rather than by the
  snapshot cadence. A missing or corrupt sidecar is logged and ignored rather than being fatal —
  losing the bookkeeping is bad, refusing to boot is worse.

- **WAL compaction could destroy the browser's persisted cache.** Compaction cleared the write-ahead
  log in one IndexedDB transaction and wrote the replacement snapshot in later ones, so an
  interruption between them left an empty WAL and no snapshot. The existing code comment anticipated
  this window; it is now closed by doing the clear and the rewrite in a **single transaction**, which
  IndexedDB commits or rolls back as a unit.

- **`ESET` would not have replicated.** `is_write_command` is a `matches!` list, which — unlike a
  `match` — has no exhaustiveness check, so a newly added command silently defaults to "not a write"
  and never reaches replicas, the AOF, or live queries. Caught while wiring `ESET`; a cross-check test
  now asserts that every command reporting written keys is also classified as a write, so the next
  addition cannot repeat it.

---

## [0.2.1] — 2026-07-19

### Fixed — Browser SDK (critical)

- **Every store operation panicked in the browser.** `core-engine` read the clock through
  `std::time::SystemTime::now()`, which is unsupported on `wasm32-unknown-unknown` and **panics**
  rather than returning an error. Because the clock is read on essentially every operation — TTL
  evaluation and LRU recency, including `Entry::new_str` on every `SET` — no write could complete in
  `recached-edge`. The panic path is present in the published `recached-edge_bg.wasm`.

  This was invisible to the existing test suite: `SystemTime::now()` works on every native target, so
  the whole native suite passed while the browser build was non-functional. It surfaced within
  minutes of standing up `wasm-bindgen-test` against headless Chrome.

  **Affected releases: 0.1.1 through 0.2.0** — every published version since the TTL feature landed.
  Confirmed by checking out `v0.2.0` unmodified and running a plain `SET` through `core-engine` in
  headless Chrome, which panics. `v0.1.0` predates the change and is unaffected. **The server is not
  affected on any version**: it runs on a native target where the clock works normally.

- **WAL compaction destroyed the persisted cache.** The same panic sat inside
  `snapshot_to_resp_cmds`, called by compaction *after* the WAL had already been cleared:

  ```rust
  JsFuture::from(idb_wal_clear_js(&db)).await?;        // WAL wiped
  let cmds = snapshot_to_resp_cmds(&store.snapshot()); // panics before rewrite
  ```

  Any client whose WAL passed `WAL_COMPACT_THRESHOLD` (1,000 entries) therefore erased its persisted
  cache and aborted before writing the replacement. The existing comment anticipated a data-loss
  window "if the tab is closed during compaction"; in practice the window was hit every time.

  Both sites now read `js_sys::Date::now()` on wasm — the clock `wasm-edge` already used elsewhere
  for client-id generation. Native builds are unchanged; `core-engine` gains a wasm-only `js-sys`
  dependency.

### Fixed — Security

- **Denial of service via `PSUBSCRIBE` pattern matching.** `server-native` carried its own recursive
  glob matcher, separate from the dynamic-programming one in `core-engine`, and used it to match
  every `PUBLISH` against every registered pattern subscription. That matcher backtracks
  exponentially: a pattern with ten wildcards against a 36-character channel name took **~7 seconds**
  (measured 69 ms → 371 ms → 1.9 s → 7.2 s as the channel grew from 24 to 36 characters, roughly 5×
  per four characters). Because `PSUBSCRIBE` patterns are supplied by the client and every publish is
  matched against all of them, a single subscriber could stall pub/sub delivery for every connected
  client. The match runs while holding the global `PubSubHub` mutex, so a slow match blocks not only
  the publishing task but every concurrent `PUBLISH`, `SUBSCRIBE`, and `PSUBSCRIBE` on the server.

  The exposure is wider than the RESP port: `PSubscribe` is classified `KeyLess` by
  `command_scope`, so it is permitted even on scope-restricted WebSocket connections — the
  untrusted-browser-client case.

  Pub/sub now uses `core_engine::store::glob_match`, the same matcher already used for sync-scope
  authorization, whose iterative implementation has no backtracking. The two were verified
  equivalent over an exhaustive differential test of **94,501 pattern/string pairs with zero
  disagreements** before the swap, so matching behaviour is unchanged. The duplicate matcher has been
  deleted, and a regression test asserts a 200-character channel matches in under 500 ms.

- **TLS silently downgraded to plaintext when half-configured.** `load_tls_acceptor` required *both*
  `RECACHED_TLS_CERT` and `RECACHED_TLS_KEY`, and returned "no TLS" if either was missing — so a
  misspelled key variable served unencrypted traffic on both the RESP and WebSocket ports, with no
  error and no failed startup. An operator who sets a certificate path intends TLS; the failure was
  undetectable until traffic had already been exposed. The server now **refuses to start** when
  exactly one of the pair is set, naming the missing variable.

- **`RECACHED_ALLOW_IPS` silently narrowed the allowlist.** Entries that failed to parse were logged
  at `warn` level and dropped. Because only exact IP addresses are supported, a natural-looking CIDR
  range such as `10.0.0.0/8` was discarded — leaving an allowlist that excluded every host the
  operator meant to admit. If *all* entries were invalid the result was an empty allowlist, which
  rejects **every** connection while the process starts normally and passes health checks. Parsing is
  now strict: an unparseable entry, or a list that yields no addresses, aborts startup with a message
  naming the offending value.

### Fixed — Browser client

- **Panic during outbox hydration on page load.** `recached-edge` held a `RefCell` borrow of its
  core state across two `await` points while renumbering restored outbox rows against IndexedDB.
  WebAssembly is single-threaded but asynchronous: a WebSocket frame arriving inside that window
  re-entered the same `RefCell` and panicked on a double borrow, taking down the client at exactly
  the moment queued offline writes were being restored — leaving them in an indeterminate state. The
  borrow is now released before any await.

  This was reachable on any reload where the socket connected while a non-empty outbox was still
  being replayed, which is the common case for a client that went offline with pending writes.

### Changed — License

- **Recached is now licensed under the Apache License 2.0** (previously MIT). Both are permissive and
  neither restricts commercial or closed-source use; Apache 2.0 adds an **explicit patent grant** from
  contributors to users — which MIT is silent on — plus a defensive patent-retaliation clause and an
  explicit statement that trademark rights are not granted. Every crate and npm package now declares
  the SPDX identifier `Apache-2.0`, and a `NOTICE` file has been added and is shipped with each
  published package.

### Added — Documentation

- **[Use Cases](docs/guide/use-cases.md)** — where Recached fits against Redis and Memcached, and
  where it does not. Opens with a disqualifying question (if no client needs to read what your
  backend writes, use Redis), covers the scenarios where the difference is real, and names Redis for
  raw single-node throughput and Memcached for large uniform blob caching. Includes a three-way
  comparison table, a "run it alongside Redis" section, and a migration checklist.
- **[Security & Production Checklist](docs/server/security.md)** — a tickable pre-exposure list, the
  authentication/TLS/allowlist details, the sync-port threat model and token-minting flow, and a
  plainly-stated list of what Recached does **not** defend against (no per-command ACLs, no audit
  log, no encryption at rest, no rate limiting on the RESP port, no third-party security review).
- **[Operations](docs/server/operations.md)** — the six exported Prometheus metrics with their types
  and labels, PromQL queries, an alert table, health-check probes, the compiled-in capacity limits,
  and backup/restore. States the observability gaps explicitly: no memory, key-count, eviction,
  replication, or sync-layer metrics exist yet, so process RSS is the only capacity signal.
- **[Troubleshooting](docs/server/troubleshooting.md)** — symptoms ordered by likelihood, derived
  from the actual error strings and limits in the code.
- **Animated architecture diagram** replacing the ASCII art in the README and `How It Works`, shipped
  as light and dark SVG variants (`<picture>` on GitHub, class-swapped in VitePress).

### Fixed — Packaging

- The workspace declared a `license` but no member crate inherited it, so `core-engine`,
  `server-native`, `sync-client`, and `wasm-edge` all published with no license metadata at all. Each
  now carries `license.workspace = true`.

### Fixed — Documentation accuracy

- **The store was described as `Arc<RwLock<HashMap>>`.** It is `Arc<DashMap<String, Entry>>` — a
  sharded concurrent map with no global lock. The old wording described a scalability bottleneck the
  code does not have, and contradicted the pipelined benchmark results on the same page.
- **Command coverage was undercounted as "~80 commands."** The real figure is **106**.
- **Two benchmark claims were contradicted by the table directly beneath them.** "Everything stays
  under 0.75 ms at p50" and "sub-millisecond p50 on every common command" both ignored the `LRANGE`
  rows at 1.96–3.58 ms. Both now scope the claim to single-key commands and name `LRANGE` as the
  exception with its real figures. The "74–96% of Redis" range was likewise corrected to 73–96% for
  single-key operations, with `LRANGE` broken out separately at 63–85%.
- **The benchmark page is now marked as measured on v0.1.8**, which predates the v0.2.0 `DEDUP`
  envelope, rather than reading as a current measurement.
- **Redis's license was listed as BSD-3.** Redis relicensed in 2024 (RSALv2/SSPLv1) and added AGPLv3
  in Redis 8; BSD-3 describes Redis ≤ 7.2 and Valkey. The comparison table now says so.
- **Pub/Sub was undocumented at the receiving end.** `subscribe`, `unsubscribe`, and `publish` were
  documented but `onMessage` — the only way to actually receive a message — appeared nowhere, making
  the API unusable from the docs alone. Now documented with an example.
- **`REPLICAOF` was missing from the command reference**, including the fact that only
  `REPLICAOF NO ONE` is accepted at runtime and re-pointing a live server requires a restart.
- **`RECACHED_ALLOW_IPS` was documented as "CIDR support depends on the version."** It accepts exact
  IP addresses only; invalid entries are logged and dropped, and an all-invalid list rejects every
  connection.
- The `isEnabled()`-style caption in the architecture diagram overflowed its viewBox and was clipped.

### Changed — Test coverage

- **`core-engine` line coverage raised from 84.19% to 93.67%** (regions 86.16% → 93.59%, functions
  86.11% → 95.49%), with tests going from 175 to **273**. Per file: `cmd.rs` 82.05% → 96.65%,
  `resp.rs` 90.71% → 97.91%, `store.rs` 84.48% → 91.89%.
- **CI now gates `core-engine` coverage at 90%** via `cargo llvm-cov --fail-under-lines 90`. The
  crate is the shared state machine — the same code evaluates commands on the server and in the
  browser through WASM — so a regression there reaches every platform at once.
- New coverage targets the paths that carry the most risk rather than chasing the number:
  - **`glob_match` gained its first direct tests.** It is not only the `KEYS`/`SCAN` matcher: it is
    the sync-scope authorization primitive (`scopes.iter().any(|p| glob_match(p, k))`), so a false
    positive is a cross-tenant read. Now covered for sibling-tenant isolation (`cart:42:*` must not
    match `cart:99:*`), `:` boundary handling, byte-wise multi-byte behaviour, and a wall-clock
    assertion that the pathological `*a*a*a…*b` shape cannot reintroduce the exponential
    backtracking the current implementation was written to remove. All passed on the first run — the
    boundary was already correct and is now pinned.
  - **Eviction and capacity**: all five eviction policies, `try_evict_for_memory`, `max_keys` with
    and without a policy, per-key eviction during `MSET`, and memory accounting for every value type.
  - **RESP parser hardening**: every prefix of a valid frame must report `Incomplete`, plus nesting
    depth limits, element caps, oversized headers, and malformed integers.
  - **Correctness edges**: expired keys invisible across all five read paths, `INCR` at `i64::MAX`
    erroring without corrupting the stored value, `WRONGTYPE` preserving the original value, `ZADD`
    `GT`/`LT` directionality, TTL overflow guards, `KEEPTTL` versus plain `SET`, and snapshot
    round-trip for every value type including TTL preservation.
  - **Per-command arity and error paths** across ~80 commands, table-driven so a single off-by-one
    guard is caught per command rather than in aggregate.
- **`server-native` line coverage raised from 55.52% to 70.18%** (regions 57.29% → 68.37%, functions
  64.81% → 75.07%), with tests going from 40 to **69**. **CI gates it at 65%.** The floor is
  deliberately lower than `core-engine`'s: most of what remains uncovered is async I/O — accept
  loops, TLS handshakes, replication streams, `main` — which the socket-driven load and chaos tests
  exercise but line coverage on this binary cannot credit. The gate protects the pure decision
  functions, not the I/O layer.
- All **107 `Command` variants** are now asserted through `command_scope`, `command_name`, and
  `primary_keys`. `command_scope` decides whether a command is scope-checked *at all* — a
  key-touching command misclassified as `KeyLess` would bypass tenant isolation silently, and
  `scopes_match` treats an empty key list as permitted. The match has no catch-all arm, so a new
  variant cannot compile until it is classified; these tests guard the remaining risk of classifying
  one wrongly. All passed on the first run — no misclassification existed. Also pinned: `DEDUP`
  inheriting its inner command's scope (otherwise a smuggling vector), multi-key commands
  (`RENAME`, `SMOVE`, `SINTERSTORE`) reporting every key they touch, and administrative commands
  mapping to `Admin` rather than `KeyLess`.
- Additional `server-native` coverage: pub/sub wire encoding (RESP3 Push frames, `pmessage` carrying
  its matching pattern, subscribe acknowledgement counts), sorted-set score formatting (`1` not
  `1.0`, infinities, the 1e15 integer-truncation boundary), `parse_memory_bytes` unit handling, and
  `parse_save_conditions` — including that one malformed pair is skipped rather than disabling
  autosave entirely.
- Prometheus metric labels are now asserted stable and lowercase for every command, since renaming
  one silently breaks existing dashboards and alerts.
- **`sync-client` line coverage raised from 80.00% to 95.77%, with every function now covered**
  (31 of 31, previously 8 uncovered); tests went from 15 to **29**. The crate is deliberately
  I/O-free, so there is no structural reason for anything in it to be untested. New coverage: sync
  scope frame construction — including that an all-empty pattern list produces **no** frame, since a
  bare `SYNC` would *clear* scopes server-side, the opposite of what an accidental empty string
  intends — live-query registration, idempotency and replay, and the `AUTH` → `SYNC` → `QSUB`
  ordering the server depends on for a scoped reconnect.
- **`wasm-edge` went from zero tests to 14**, covering `snapshot_to_resp_cmds` — the WAL encoder that
  determines whether browser-persisted data survives a page reload. Pinned: lists round-trip through
  `RPUSH` rather than `LPUSH` (which would silently reverse them), already-expired entries are
  dropped rather than resurrected, empty collections emit nothing (`RPUSH key` with no members is a
  syntax error that would abort hydration part-way), strings carry a *relative* `PX` while
  collections get a separate absolute `PEXPIREAT`, and rate-limiter state never reaches the browser
  WAL. The strongest test replays an encoded snapshot through the real `core-engine` and asserts
  every value type comes back intact.

- **Browser tests, via `wasm-bindgen-test` in headless Chrome** — a new `browser-tests` CI job runs
  `wasm-pack test --headless --chrome wasm-edge`. 13 tests cover the IndexedDB persistence layer:
  object-store creation across the v2/v3 upgrades, WAL append and sequence ordering, compaction
  clearing the WAL while **preserving the outbox** (wiping it would discard writes still awaiting
  server acknowledgement), outbox durability across a database reopen, delete/replace semantics, and
  `meta` round-tripping the client identity and session epoch that make delivery exactly-once. A
  further test exercises `core-engine` itself on a real wasm target — the regression guard for the
  clock panic above, which no native test can catch.

  Still uncovered: the WebSocket and reconnect paths, which need a live server running alongside the
  browser. Those remain follow-up work.
- **CI no longer excludes `wasm-edge` from lint and test.** The exclusion was documented as "requires
  the wasm32 target to compile", which was not accurate — the crate builds and tests natively. The
  real blocker was four clippy findings, one of which was the `RefCell`-across-await panic fixed
  above. Excluding the crate is what allowed that bug to persist; all four are resolved and the
  workspace now lints and tests with nothing skipped.

---

## [0.2.0] — 2026-07-11

### Changed

- **New `sync-client` crate** — the client-side sync brain (durable outbox, `DEDUP` envelopes, ordered-reply acknowledgment correlation, session re-establishment, reconnect backoff) extracted from `wasm-edge` into a platform-neutral, I/O-free crate with an effects-based API. `recached-edge` is now a thin browser adapter (WebSocket, IndexedDB, `setTimeout`) over it, and the planned mobile bindings (roadmap #6) will reuse it unchanged — merge semantics can never drift between platforms. The previously untestable connection logic now has native unit tests (replay ordering, ack retirement, dedup id/epoch math, outbox restore renumbering, backoff schedule). The wire protocol is now specified normatively in `docs/server/protocol.md`. (`sync-client/`, `wasm-edge/src/lib.rs`)

### Added

- **Offline-first writes with automatic reconnection** (`recached-edge`) — the browser client now survives connection loss without glue code. Writes made offline apply locally and queue as *operations* (capped at 10 000, FIFO); the client reconnects with exponential backoff (500 ms → 30 s cap) and re-establishes the session in order: `AUTH` → `SYNC TOKEN`/scopes → re-subscribe every live query (fresh `qstate` re-hydrates local state) → replay the queue. Because operations (not final values) replay, merges follow the data type: new `incr`/`decr` queue INCRBY **deltas** that merge additively with concurrent increments (PN-counter semantics — verified E2E: 3 online + 2 offline-queued = 5), `jmerge` patches deep-merge, collection ops replay, and plain `set` is last-writer-wins by server arrival. New API: `incr`/`decr`, `disconnect()`, `createCache({ connect: { reconnect } })`. The queue is a **durable outbox**: with persistence enabled it is mirrored to a new IndexedDB object store (DB schema v2, upgraded in place), restored on startup, and re-sent on the next connect — offline writes survive full page reloads. Writes are retired from the outbox only when the server's reply acknowledges them (replies are ordered, so acknowledgment is exact; the WAL compactor was split off onto a WAL-only clear so compaction can't wipe pending writes). Delivery is **exactly-once**: every store write carries a new `DEDUP client-id id` envelope — the client id is `crypto.randomUUID()` persisted in IndexedDB (meta store, DB schema v3), write ids are monotonic with a persisted session epoch in the upper bits so they stay monotonic across reloads — and the server skips ids at or below the client's high-water mark, replying `+DUP` (which still retires the outbox row). Verified E2E: an increment replayed on a new connection after a lost acknowledgment returns `+DUP` and does not double-apply. Scope checks, replica rejection, and metrics delegate to the wrapped command; pub/sub commands are deliberately not dedup-wrapped (skipping a replayed SUBSCRIBE would drop the subscription). Residual caveat documented: dedup marks are in server memory (swept after 24 h idle), so a server restart inside the ack window can admit one duplicate. LWW is arrival-order — documented. (`wasm-edge/src/lib.rs` — connection layer rebuilt around a reconnect-capable shared state, `wasm-edge/sdk.ts`, `docs/browser/offline.md`)

- **Native JSON type (`JSET` / `JGET` / `JMERGE`)** — JSON documents stored parsed (`serde_json`), no RedisJSON module needed. `JSET key path value` writes one location (`$`, `$.user.name`, `$.items[2]`; deterministic paths — no wildcards; intermediate objects auto-created); `JGET key [path]` reads serialized (sorted keys, deterministic); `JMERGE key patch` applies RFC 7386 Merge Patch (recursive object merge, `null` removes fields, `null` patch deletes the key). Writes replicate as replayable commands, so only the change travels to replicas/AOF — and to browsers: `recached-edge` gains `jset`/`jget`/`jmerge` (with `JSON.stringify`/`parse` handled) and applies incoming `JSET`/`JMERGE` pushes natively, so a merge from any client updates every connected browser's local document. `TYPE` reports `json`; snapshot format gains a trailing variant (old snapshots remain readable); live queries deliver a `json` type marker (read the document with `JGET`). (`core-engine/src/cmd.rs`, `core-engine/src/store.rs`, `server-native/src/main.rs`, `wasm-edge/src/lib.rs`, `wasm-edge/sdk.ts`)

- **Live queries, client half** — `recached-edge` gains `liveQuery(pattern)` (ref-counted per pattern; returns a stop function), `getMatching(pattern)` (sorted local snapshot), `syncToken`/`syncScopes` (also settable via `createCache({ connect: { syncToken } })`), and the WebSocket handler now applies `qstate` initial-state replies and `keychange` diffs (string sets, nil deletions) into the local WASM store. `@recached/react` and `@recached/vue` gain `useKeys(pattern)` — current matching keys plus live updates in one line, `useSyncExternalStore`-safe with stable snapshots. (`wasm-edge/src/lib.rs`, `wasm-edge/sdk.ts`, `sdks/recached-react/src/useKeys.ts`, `sdks/recached-vue/src/useKeys.ts`)
- **Live queries (`QSUB` / `QUNSUB`)** — the server-side primitive for reactive UIs. `QSUB pattern` (WebSocket-only) replies with `["qstate", pattern, ...]` — the current state of every key matching the glob pattern as flat key/value pairs (capped at 10 000 keys) — then streams every mutation to matching keys — including keys created after subscribing — as `keychange` pushes, with deletions as nil values. Subscriptions register *before* the initial snapshot is taken, so a write landing in between is delivered as an idempotent diff rather than lost. Pushes travel on a dedicated channel so they never dirty `WATCH` transactions, and pattern watchers piggyback on the existing watch-hub atomic fast path (zero cost per write when no live queries exist). Under strict sync scoping, requested patterns must sit inside the connection's granted scopes (prefix-style cover: a `cart:*` grant admits `QSUB cart:42:*`). Limits: 64 live queries per connection. (`core-engine/src/cmd.rs`, `core-engine/src/store.rs` — new `matching_key_values`, `server-native/src/main.rs`)
- **Scoped sync and per-client auth** — previously every mutation was pushed to every connected WebSocket client, so any browser could observe the whole keyspace. Each WS connection can now declare glob-pattern scopes with the new `SYNC` command, and the mutation fan-out delivers only matching keys (the broadcast payload now carries the touched keys, so filtering needs no re-parsing). With `RECACHED_SYNC_SECRET` set (strict mode), scopes become an authorization boundary: connections must present an HMAC-SHA256-signed scope token (`SYNC TOKEN <token>`, minted by the application backend, optional expiry) before receiving any pushes or running any key command; every command — reads included — is then checked against the granted scopes at execution *and* at MULTI queue time, and keyspace-wide/administrative commands are refused. Without the secret, behavior is backward compatible (unscoped connections receive everything; literal `SYNC <patterns>` acts as a bandwidth filter). TCP connections are unaffected. See `docs/server/sync-scopes.md`. (`core-engine/src/cmd.rs`, `core-engine/src/store.rs`, `server-native/src/main.rs`)
- **Built-in sliding-window rate limiting** — `RLSET key limit window` configures a limiter (persists until `DEL`/`EXPIRE`); `RLCHECK key [limit window]` records an attempt and returns `[allowed, remaining, retry_after_ms]`, mapping directly onto `X-RateLimit-*` / `Retry-After` HTTP headers. The optional inline `limit window` pair creates the limiter on first use for per-IP/per-user keys, and such auto-created limiters expire one window after the last attempt so they self-clean. Internally a new `ratelimit` entry type stores attempt timestamps in a monotonic deque — O(1) amortized per check, length bounded by `limit` (denied attempts are not recorded). `RLSET` config replicates to AOF/replicas; attempt state is deliberately transient. Snapshot format gains a new trailing enum variant (old snapshots remain readable). (`core-engine/src/cmd.rs`, `core-engine/src/store.rs`, `server-native/src/main.rs`, `docs/server/commands.md`)

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
