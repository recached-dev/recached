# recached-embed

**Hold a live slice of a Recached cache in your Rust service's own memory.**

`recached-edge` lets a *browser* keep a local copy of the cache and receive pushes
when it changes. This crate is the same thing for a *server*: reads come from
local memory, and the WebSocket carries only writes out and change notices in.

```rust
use recached_embed::Cache;

let cache = Cache::connect("ws://127.0.0.1:6380").await?;

// Once, at start-up. Returns after initial state lands, so the first
// request never races hydration.
cache.watch("fare:*").await?;

// Hot path: no network, no await, no lock.
let fare = cache.get("fare:MNL-CEB")?;
```

## Why

Config-shaped data — fare tables, feature flags, tenant settings, entitlement
checks, rate-limit tiers — is read on nearly every request and changes a few
times a day. The usual options are all bad: hitting Postgres every request is
too slow; putting it in Redis is still a round-trip per read; caching it in a
local `HashMap` means your other five pods serve stale data forever.

So everyone hand-writes "local `HashMap` + pub/sub listener that clears
entries." The hard part was never the `HashMap` — it's *"my listener was
disconnected for eight seconds during a deploy; what did I miss?"* Usually
nothing detects it, and that pod stays stale until it restarts.

This crate is that pattern with the reconnect path actually handled: on
reconnect the client re-subscribes and re-hydrates, so a missed window heals
instead of going quietly stale.

## Fit

**Good for:** small, hot, shared, read-heavy, *slowly-changing* data.

**Wrong for:** data that changes constantly (live counters, seat inventory),
working sets that do not fit in every process, keys read once, or anything
where being stale by milliseconds is unacceptable — payments, seat locks,
inventory decrement. Use the server directly for those.

## Hydration is part of the contract

The local store holds only what your `watch()` patterns hydrated. Reading
anything else returns `Error::NotHydrated`, never `None`:

```rust
cache.watch("fare:*").await?;

cache.get("fare:MNL-CEB")?;   // Ok(Some(..)) or Ok(None) — both authoritative
cache.get("user:42")?;        // Err(NotHydrated) — NOT a silent None
```

A silent `None` here is indistinguishable from "the key does not exist", which
is exactly how an embedded cache serves confidently wrong answers. If you would
rather pay for a round-trip than declare a pattern, use `get_or_fetch()`.

## Connection

Connect to the **WebSocket sync port (6380)**, not RESP on 6379. Live queries
and change pushes travel the sync path; a plain TCP client is never sent
keychange notifications and cannot hold a coherent local copy.

```rust
let cache = Cache::builder("ws://cache.internal:6380")
    .password(std::env::var("RECACHED_PASSWORD")?)
    .connect()
    .await?;
```

Reconnection is automatic with jittered exponential backoff. Writes issued while
disconnected are queued in the outbox and replayed on reconnect (they return
`Error::Disconnected` — unacknowledged, not lost). Local reads keep working
throughout; they simply stop receiving updates, so check `is_connected()` if
staleness during an outage matters to you.

## Known limitations

These are upstream behaviours in the sync protocol, not choices this crate
makes. Both affect `recached-edge` in the browser identically. Each has a
regression test in `tests/live.rs`, marked `#[ignore]` with the details.

- **TTLs converge within ~1s, not instantly.** A local copy does not expire on
  its own clock; it learns of the expiry when the server's once-per-second
  sweep removes the key and announces it as a delete. Fine for session caches;
  compare a stored deadline yourself if you need an exact instant.
- **A client offline when a key expires keeps it.** `qstate` re-hydration adds
  keys but never removes local keys absent from the snapshot, so a key that
  expired or was deleted during a disconnect survives reconnection until
  something writes to it again. Applies to `DEL` as much as to expiry.
- **Collections do not hydrate on connect.** `qstate` sends collections as bare
  type-name markers rather than contents, and the client drops them. A hash,
  list, set, zset or JSON key written *before* you connect stays invisible until
  its next write. Live updates after that point are complete. String keys are
  unaffected.

## Testing

```sh
cargo run -p recached --bin recached-server &
RECACHED_EMBED_TEST_URL=ws://127.0.0.1:6380 cargo test -p recached-embed
```

Without the env var the live suite skips, so `cargo test` stays green with no
server running.

## Example

```sh
cargo run -p recached-embed --example two_pods
```

Two caches, one server: pod B sees pod A's write without ever asking for it.
