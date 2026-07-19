# Persistence

::: warning Requires recached-edge 0.2.1 or later
On 0.1.1 – 0.2.0, write-ahead-log compaction cleared the WAL and then panicked before writing the
replacement snapshot, destroying the persisted cache. See
[Getting Started](/browser/getting-started) for the full detail.
:::

By default, the `recached-edge` cache lives in WASM memory and is lost on page refresh. Enabling persistence writes a WAL (write-ahead log) to IndexedDB, so the cache is hydrated immediately on the next page load — before any network request completes.

## How the IndexedDB WAL works

The WAL is an IndexedDB object store named `wal` inside a versioned database (default name: `recached`). Every cache mutation — whether it came from a local write or a server push — is appended as a sequential entry:

```typescript
interface WalEntry {
  seq: number          // Monotonically increasing sequence number
  command: string      // RESP-encoded command, e.g. "*3\r\n$3\r\nSET\r\n..."
  timestamp: number    // Unix ms timestamp of the write
}
```

The WAL is an append-only log. Reads do not touch it. Only writes (`SET`, `DEL`, `HSET`, `EXPIRE`, etc.) append entries.

---

## Enabling persistence

Pass `persistence: true` to `createCache()`:

```typescript
import { createCache } from 'recached-edge'

const cache = await createCache({
  persistence: true,
  connect: { url: 'ws://localhost:6380' },
})
```

For a local-only cache (no server), omit `connect`:

```typescript
const cache = await createCache({ persistence: true })
```

---

## The page-load hydration flow

When `enable_persistence()` is called and the page loads:

1. **Open IndexedDB.** The WAL database is opened (created on first use).
2. **Replay the WAL.** All entries are read in sequence order and applied to the in-memory `core-engine` store. This is a synchronous WASM operation — fast, no network.
3. **Hydration complete.** The local store now reflects the last known cache state.
4. **Connect to the server.** The WebSocket connection is opened. The server pushes mutations that occurred while the page was closed. These are applied on top of the hydrated state.
5. **UI renders.** By the time step 4 begins, the cache already has data from step 2. Components that read from the cache during or after hydration see stale-but-recent data immediately, then receive live updates as the WebSocket sync catches up.

The effect from a user's perspective: the UI shows data instantly on page load, without waiting for the server round-trip.

---

## The refresh flow

Here is the timeline for a page refresh with persistence enabled:

```
t=0ms   Page starts loading
t=?ms   WASM binary fetched and compiled
t=?ms   enable_persistence() called — IndexedDB opens
t=?ms   WAL replay begins (local reads, fast)
t=?ms   WAL replay complete — local store populated
t=?ms   UI renders with cached data  ← user sees content here
t=?ms   WebSocket connection opens
t=?ms   Server pushes missed mutations — store updates
t=?ms   UI re-renders with live data  ← incremental updates
```

For most applications, the WAL replay finishes in < 10 ms even with thousands of entries. The user sees their data before the WebSocket handshake is complete.

---

## WAL compaction

The WAL grows with every write. Without compaction, it would grow indefinitely, and hydration would become slower over time.

Compaction runs automatically after a successful WebSocket sync. The process:

1. After the WebSocket connection is established and the server has sent its current mutations, `wasm-edge` takes a snapshot of the current store state.
2. The snapshot is serialized as a compact set of `SET` / `HSET` / `RPUSH` etc. commands — one per key.
3. The existing WAL entries are replaced with the snapshot entries.
4. Future writes append to the compacted WAL.

After compaction, hydration replay time is bounded by the number of keys in the store, not the number of mutations ever made.

---

## `clearPersistence()`

Call this on sign-out or when you need to reset the local cache state completely.

```typescript
await cache.clearPersistence()
```

This deletes the IndexedDB database. The next page load starts with an empty local store and syncs fresh from the server.

### Sign-out pattern

```typescript
async function signOut() {
  // Clear the persisted WAL so the next session starts clean
  await cache.clearPersistence()

  // Redirect to login
  window.location.href = '/login'
}
```

Do not skip `clearPersistence()` if you handle multiple user accounts on the same device. Without it, the next user's session would hydrate with the previous user's cached data.

---

## Limitations

### TTL replay

When the WAL is replayed on page load, TTL values are replayed as their original expiry timestamps (`EXPIREAT` with the original absolute timestamp), not as relative `EXPIRE` values. This means:

- A key with a 60-second TTL set 10 seconds before page close will have 50 seconds remaining on hydration — correct behavior.
- A key that expired while the page was closed will be absent from the hydrated store — also correct.

TTL replay is accurate as long as the system clock has not been adjusted significantly.

### No delta sync yet

Recached does not yet implement `SYNC <seq>` — a protocol for the server to replay only the mutations that occurred after a given sequence number on WebSocket reconnect. This is on the roadmap.

Currently, when the WebSocket reconnects after a gap, the server replays all keys from its current state (a full resync), not just the mutations that occurred while the browser was offline. For most use cases this is fine — the full resync takes milliseconds. If you have very high write rates and very large key sets, this may result in a brief period of stale data after reconnect.

### Persistence is per-origin

IndexedDB is scoped to the browser origin (`scheme + host + port`). Data persisted on `https://app.example.com` is not accessible on `https://staging.example.com`. This is the standard browser storage isolation model.

### Not a replacement for a database

The WAL records cache mutations, not ground-truth application data. If the Recached server is restarted without being repopulated from a database, the next browser sync will receive an empty server state and overwrite the hydrated local store with empty data. The server is the authoritative source; IndexedDB persistence is a fast-read optimization, not durable storage.
