# Changelog

All notable changes to Recached are documented here.

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
