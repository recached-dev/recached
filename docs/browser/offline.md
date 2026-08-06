# Offline & Reconnection

Browsers go offline. Recached is built so that when they do, the app keeps working — and when the connection returns, state converges without glue code.

## What happens automatically

**While connected**, every local write applies instantly to WASM memory and streams to the server.

**When the connection drops:**

- Reads keep working — they never left local memory to begin with.
- Writes keep working: they apply locally and queue as *operations* in a durable outbox (up to 10 000; beyond that the oldest queued write is dropped with a console warning). With persistence enabled, the outbox lives in IndexedDB — offline writes survive a full page reload and still reach the server.
- The client reconnects with exponential backoff: 500 ms, doubling to a 30 s cap.

**When the connection returns**, the client re-establishes the session in order:

1. `AUTH` (the password is remembered)
2. `SYNC TOKEN` / sync scopes (remembered)
3. Every active live query is re-subscribed — the fresh `qstate` re-hydrates local keys with whatever happened server-side while you were away
4. The outbox replays FIFO

A queued write is retired from the outbox only when the server's reply *acknowledges* it (replies arrive in command order, so acknowledgment is exact). A write that was sent but unacknowledged when the connection died is re-sent on reconnect — and delivery is **exactly-once**: every store write carries a `DEDUP` envelope (a per-client id plus a monotonic write id), and the server skips any id it has already applied, replying `+DUP` so the outbox still retires the entry. A replayed `incr` whose acknowledgment was lost can never apply twice.

The client identity behind this is persisted with the outbox (IndexedDB), and each session's ids start above the previous session's, so exactly-once holds across page reloads too. One caveat: the server's dedup marks are in-memory — a *server* restart in the exact window between applying a write and the client receiving the acknowledgment reopens a narrow duplicate possibility.

Nothing to call, nothing to configure. Disable with `createCache({ connect: { reconnect: false } })` or stop a connection deliberately with `cache.disconnect()`.

## Merge semantics — what happens to conflicting writes

Recached queues *operations*, not final values. That choice decides how offline changes merge with concurrent changes from other clients:

| Write type | Offline behavior | Merge result |
|---|---|---|
| `incr` / `decr` | queues the **delta** (`INCRBY`) | **Additive** — your +2 and their +3 make +5, nobody's counts are lost (PN-counter semantics) |
| `sadd` / `srem`-style collection ops | queues the operation | Operations replay — an offline `SADD` survives a concurrent server-side change to the same set |
| `jmerge` | queues the **patch** | Deep-merges into the current document — fields others changed while you were offline are preserved unless your patch touches them |
| `set` / `del` / `jset` | queues the command | **Last-writer-wins by arrival at the server** — your offline write overwrites the value when it replays |

Use the operation forms when concurrent edits matter: `cache.incr('cart:count')` instead of read-modify-`set`, `cache.jmerge(key, patch)` instead of `jset(key, '$', wholeDoc)`. The wire format is the same either way — the semantics are not.

```ts
// ❌ read-modify-write: offline, this clobbers everyone else's increments
cache.set('cart:count', String(Number(cache.get('cart:count')) + 1))

// ✅ delta: merges additively no matter who else incremented meanwhile
cache.incr('cart:count')
```

## Limits to know about

- **Durability requires persistence.** Without `persistence: true`, the outbox is in-memory: offline writes replay within the tab session but are lost on reload. With it, unacknowledged writes are restored from IndexedDB on startup and re-sent on the next connect.
- **Exactly-once depends on server memory.** Dedup high-water marks live in server memory (swept after 24 h idle); a server restart at exactly the wrong moment can let one duplicate through — see above.
- **LWW means arrival order, not wall-clock order.** A `set` replayed from a client that was offline for an hour overwrites the server's newer value for that key. Prefer operation forms for anything multiple parties write.
- `clearPersistence()` (sign-out) discards unsent offline writes along with the local state.
- Reconnection uses `window.setTimeout` — in non-browser environments without a `window`, auto-reconnect is inactive.
- **The outbox also fills in local-only mode.** A cache created with `persistence: true` but no `connect` still records a row per write for a replay that can never happen, and warns `offline write queue full` past 10,000 of them. Nothing is lost — the store and its WAL are unaffected — but the queue is pure overhead there. See [no server at all](/guide/use-cases#no-server-at-all-the-client-cache-on-its-own).
