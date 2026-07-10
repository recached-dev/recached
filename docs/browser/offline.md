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

A queued write is retired from the outbox only when the server's reply *acknowledges* it (replies arrive in command order, so acknowledgment is exact). A write that was sent but unacknowledged when the connection died is re-sent on reconnect — delivery is **at-least-once**: in the narrow window where the server processed a write but the acknowledgment was lost, a replayed `incr` can apply twice. Exactly-once delivery (deduplication ids) is future work.

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
- **Delivery is at-least-once**, not exactly-once — see above. For counters this means a rare double-increment is possible when a connection dies at exactly the wrong moment.
- **LWW means arrival order, not wall-clock order.** A `set` replayed from a client that was offline for an hour overwrites the server's newer value for that key. Prefer operation forms for anything multiple parties write.
- `clearPersistence()` (sign-out) discards unsent offline writes along with the local state.
- Reconnection uses `window.setTimeout` — in non-browser environments without a `window`, auto-reconnect is inactive.
