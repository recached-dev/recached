<div align="center">
  <img src="recached.jpg" alt="Recached" width="800" />
  <h1>Recached ⚡</h1>
  <p><b>A Rust cache server that runs on your backend <em>and</em> inside the browser.</b></p>

  <a href="#"><img src="https://img.shields.io/badge/Language-Rust-orange.svg" alt="Rust"></a>
  <a href="#"><img src="https://img.shields.io/badge/Architecture-Multi--Core-blue.svg" alt="Multi-Core"></a>
  <a href="#"><img src="https://img.shields.io/badge/Ecosystem-WebAssembly-yellow.svg" alt="Wasm"></a>
  <a href="#"><img src="https://img.shields.io/badge/License-MIT-green.svg" alt="MIT"></a>
</div>

---

**Recached** is an in-memory cache written in Rust with one idea that existing caches don't have: it compiles to WebAssembly so the same cache engine runs natively on your server *and* directly inside the browser or edge runtime, with the two sides kept in sync over WebSockets.

On the backend it speaks RESP, so any Redis client works against it today. In the browser, you import it as a `.wasm` module and get zero-latency local reads with automatic background sync to the server — no extra round-trips, no polling, no external state management library.

> [!NOTE]
> Recached is not a full Redis replacement — no persistence, replication, or Lua scripting. It implements the subset most applications actually use: strings, expiry, counters, all collection types (Hash/List/Set/Sorted Set), transactions, pub/sub, and observable keys over WebSocket. Best fit: reactive UIs, session caches, browser-side API response caching, and rate limiting.

---

## Why Recached exists

Every caching solution today forces a choice: put the cache on the server (latency on every read) or duplicate state in the client (stale data, cache invalidation hell). Recached removes that choice.

The `core-engine` crate is a pure Rust state machine with no network dependencies. It compiles to native code for the server and to `.wasm` for the browser. Both run the same logic. The WebSocket sync layer keeps them consistent — a `SET` on the server pushes to all connected browser instances automatically.

```
┌─────────────────┐        RESP (port 6379)        ┌──────────────────┐
│   Your backend  │ ──────────────────────────────► │  Recached Server │
└─────────────────┘                                 │  (server-native) │
                                                    └────────┬─────────┘
                                                             │ WebSocket
                                                             │ sync (6380)
                                                    ┌────────▼─────────┐
                                                    │  Browser / Edge  │
                                                    │  (wasm-edge)     │
                                                    │  local reads: 0ms│
                                                    └──────────────────┘
```

---

## Use Cases

### 1. Live UI that stays in sync without polling

**The problem today:** A user opens a dashboard — cart count, active users online, live stock ticker. The frontend either polls every few seconds (wasted requests, always slightly stale) or you wire up a custom WebSocket server plus a state management library (Redux, Zustand, Recoil) just to keep one number fresh. Every engineer on the team ends up maintaining two caches: one on the server and one in the client store.

**With Recached:** Your backend does `SET cart:user:42 3` over RESP as normal. Every browser tab connected to the WebSocket port receives that mutation automatically — the WASM local cache updates instantly. The frontend reads `cache.get("cart:user:42")` and gets `3` in 0 ms. No polling loop. No client-side store. No sync code to write.

---

### 2. Live inventory and seat counts without stale reads

**The problem today:** Flash sales, event ticketing, limited-drop products — anything where "only 3 left" has to actually mean 3. The frontend either polls every few seconds (users see stale counts, oversells happen) or you accept the latency of a server round-trip on every render. Solving it properly means SSE or a custom WebSocket layer just for inventory deltas, on top of your existing cache.

**The fit for in-memory:** Inventory counts in the cache are intentionally short-lived. The database is the source of truth; the cache is the fast read layer. If the server restarts, your backend repopulates from the DB. Ephemeral is fine here — correctness comes from the server, speed comes from Recached.

**With Recached:** Your backend decrements stock — `DECR inventory:item:99` — and every browser tab showing that product page sees the updated count pushed instantly via WebSocket sync. The frontend reads from local WASM memory (0 ms). No polling, no separate SSE endpoint, no oversell from stale browser state.

---

### 3. Shared real-time state across browser tabs and users

**The problem today:** Collaborative features — a shared whiteboard cursor, a "who's online" indicator, a live vote count — require either a dedicated pub/sub service (Pusher, Ably, Socket.io) with its own SDK and billing, or a hand-rolled WebSocket server that you now have to operate. Client-side state is a separate layer on top of all that.

**With Recached:** Pub/sub works over the same WebSocket port your cache already uses. One browser publishes `cache.publish("cursors", JSON.stringify({x, y}))`. All other tabs subscribed to `"cursors"` receive it — including server-side subscribers on port 6379. Shared counters (`INCR votes:poll:1`) are automatically consistent across every connected client. One server, one connection, cache + pub/sub in the same primitive.

---

### 4. Frontend-only API response cache — no server required

> **No backend setup needed.** Just import the WASM module and go.

**The problem today:** Frontend apps built with React, Vue, or Svelte reach for Zustand or Redux to hold server data — but those libraries have no concept of expiry. You end up writing your own stale-check logic: storing a `fetchedAt` timestamp next to every piece of data, comparing it on every read, and manually invalidating on user action. As the app grows, this boilerplate spreads across every slice of state. React Query and SWR help, but they are full framework-level dependencies with their own mental model.

**Recached is different here:** `connect()` is optional. If you never call it, the WASM module runs entirely inside the browser as a pure local in-memory cache — no Recached server, no Redis, no backend changes. TTL is a first-class primitive, not something you bolt on.

```js
import init, { RecachedCache } from 'recached-edge';

await init();
const cache = new RecachedCache(); // no connect() — purely local, no server needed

async function getProducts() {
  const cached = cache.get('products');
  if (cached) return JSON.parse(cached);

  const data = await fetch('/api/products').then(r => r.json());
  cache.set_ex('products', JSON.stringify(data), 300); // expires in 5 minutes
  return data;
}

// Works the same way for any API call
async function getUser(id) {
  const key = `user:${id}`;
  const cached = cache.get(key);
  if (cached) return JSON.parse(cached);

  const user = await fetch(`/api/users/${id}`).then(r => r.json());
  cache.set_ex(key, JSON.stringify(user), 60); // expires in 60 seconds
  return user;
}
```

**What you get without any server:**
- `set_ex(key, value, seconds)` — cache with built-in TTL, no timestamp tracking
- `get(key)` — returns `null` automatically when expired, 0 ms when fresh
- `exists(key)` / `ttl(key)` — check cache state without a fetch
- `del(key)` — manual invalidation on mutation (form submit, optimistic update)
- Full Redis collection types — cache a list, a hash, a sorted set, not just strings
- Zero extra dependencies beyond the `.wasm` file

**vs Zustand / Redux for data fetching:** You stop writing `if (Date.now() - state.fetchedAt > 300_000)` in every selector. TTL is declared once at write time and enforced automatically. Recached does not replace Zustand or Redux for UI state — it replaces the manual caching layer you built on top of them.

---

## Getting started

### Run the server

```bash
# Docker
docker run -p 6379:6379 -p 6380:6380 ghcr.io/thinkgrid-labs/recached:latest

# Homebrew (macOS)
brew tap thinkgrid-labs/recached && brew install recached && recached-server

# Cargo
cargo install recached && recached-server
```

### Use from your backend (any Redis client, port 6379)

```javascript
import Redis from 'ioredis';

const cache = new Redis('redis://127.0.0.1:6379');
await cache.set('user:1', 'Alice');
console.log(await cache.get('user:1')); // "Alice"

// Collections work too
await cache.hset('session:42', 'user', 'Alice', 'role', 'admin');
await cache.lpush('queue:jobs', 'task-1', 'task-2');
await cache.sadd('tags:post:1', 'rust', 'wasm', 'cache');
await cache.zadd('leaderboard', 100, 'alice', 200, 'bob');

// Pub/Sub
const sub = new Redis('redis://127.0.0.1:6379');
sub.subscribe('events');
sub.on('message', (channel, message) => console.log(channel, message));
await cache.publish('events', 'hello');
```

### Use from the browser (WebAssembly, port 6380)

```javascript
import init, { RecachedCache } from 'recached-edge';

await init();
const cache = new RecachedCache();

// Connects to the server and syncs state changes in the background
cache.connect('ws://127.0.0.1:6380');

cache.set('theme', 'dark');        // writes locally + pushes to server
console.log(cache.get('theme'));   // reads from local WASM memory — 0ms

// Subscribe to server-side pub/sub channels
cache.subscribe('notifications');
// Publish from the browser to all subscribers
cache.publish('events', 'user-clicked');
```

Any mutation on the server side (`SET`, `DEL`, `HSET`, `LPUSH`, etc.) is automatically pushed to all connected browser instances. Any write from the browser is pushed to the server and fanned out to other clients.

**Observable keys** — receive a push whenever a specific key changes, from any client:

```javascript
// RESP over WebSocket — works with any raw WebSocket client
const ws = new WebSocket('ws://127.0.0.1:6380');

// Watch a key — server sends a `keychange` push on every mutation
ws.send('*2\r\n$5\r\nWATCH\r\n$12\r\ncart:user:42\r\n');

ws.onmessage = ({ data }) => {
  // Push format: ["keychange", "key-name", "new-value-or-type-hint"]
  // String keys: third element is the current value
  // Complex types (hash/list/set/zset): third element is the type name — re-fetch with HGETALL etc.
  // Deleted keys: third element is nil ($-1)
};

// Stop watching
ws.send('*2\r\n$7\r\nUNWATCH\r\n$12\r\ncart:user:42\r\n');
// UNWATCH with no args clears all watches for this connection
ws.send('*1\r\n$7\r\nUNWATCH\r\n');
```

---

## Configuration

```bash
RECACHED_PASSWORD="secret"          \  # require AUTH; disconnects after 5 wrong attempts
RECACHED_ALLOW_IPS="127.0.0.1"     \  # comma-separated allowlist (invalid entries are logged + skipped)
RECACHED_MAX_KEYS="1000000"         \  # hard key cap; SET errors when reached
RECACHED_METRICS_PORT="9091"        \  # Prometheus metrics port (default 9091); scrape at /metrics
RECACHED_TLS_CERT="/path/to/cert.pem" \  # PEM cert file; enables TLS on both ports when set with KEY
RECACHED_TLS_KEY="/path/to/key.pem"   \  # PEM private key file; falls back to plain TCP if either is unset
RECACHED_EVICTION="lru"             \  # eviction when max keys hit: lru / allkeys-random / volatile-lru / volatile-ttl (default: noeviction)
RUST_LOG="info"                     \  # log level: error / warn / info / debug
recached-server
```

---

## Architecture

Three crates with hard dependency boundaries:

| Crate | Role |
|---|---|
| `core-engine` | Pure state machine — no networking, no I/O. RESP parser (depth-limited), typed command dispatch, `Arc<RwLock<HashMap>>` store with `EntryValue` enum (Str/Hash/List/Set/ZSet), TTL engine, optional key cap. Compiles to both native and `wasm32`. |
| `server-native` | Tokio TCP server (port 6379) + WebSocket server (port 6380). Persistent read buffers handle fragmented RESP. Per-connection pub/sub delivery via `mpsc` channels. Connection semaphore, auth rate-limiting, sender-ID broadcast filter, structured `tracing` logging. |
| `wasm-edge` | `wasm-bindgen` JS bindings. Local zero-latency reads, RESP-over-WebSocket sync with the server. Closure lifecycle managed correctly — reconnecting doesn't leak memory. |

---

## What works today

**Protocol & server**
- RESP protocol — full parser/serializer, handles fragmentation, depth-limited (no stack-overflow DoS)
- TCP (port 6379) compatible with any Redis client
- WebSocket sync (port 6380) between server and browser WASM instances
- Sender-ID filter: browser clients don't double-apply their own mutations
- `RECACHED_PASSWORD` + brute-force lockout after 5 failures
- `RECACHED_ALLOW_IPS` with validated IP parsing
- `RECACHED_MAX_KEYS` memory cap
- Connection semaphore (max 1024 concurrent)
- Background active eviction (1s sweep) + lazy eviction on every read
- Structured `tracing` logs

**Commands**

*Core*
- `PING`, `AUTH`

*Strings*
- `SET` (with `EX`/`PX`/`EXAT`/`PXAT`/`NX`/`XX`/`KEEPTTL`/`GET`), `GET`, `GETSET`
- `MGET`, `MSET`, `SETNX`, `SETEX`, `PSETEX`
- `APPEND`, `STRLEN`
- `INCR`, `DECR`, `INCRBY`, `DECRBY`

*Expiry*
- `EXPIRE`, `PEXPIRE`, `EXPIREAT`, `PEXPIREAT`
- `TTL`, `PTTL`, `PERSIST`

*Keys*
- `DEL`, `UNLINK`, `EXISTS`, `TYPE`, `RENAME`
- `KEYS`, `SCAN`, `DBSIZE`, `FLUSHDB`

*Hash*
- `HSET`, `HGET`, `HGETALL`, `HDEL`, `HMGET`
- `HKEYS`, `HVALS`, `HLEN`, `HEXISTS`, `HSETNX`
- `HINCRBY`, `HINCRBYFLOAT`

*List*
- `LPUSH`, `RPUSH`, `LPUSHX`, `RPUSHX`
- `LPOP`, `RPOP`, `LRANGE`, `LLEN`, `LINDEX`
- `LSET`, `LREM`, `LTRIM`

*Set*
- `SADD`, `SMEMBERS`, `SREM`, `SCARD`, `SISMEMBER`, `SMISMEMBER`
- `SINTER`, `SINTERSTORE`, `SUNION`, `SUNIONSTORE`, `SDIFF`, `SDIFFSTORE`
- `SPOP`, `SRANDMEMBER`, `SMOVE`

*Sorted Set*
- `ZADD` (with `NX`/`XX`/`CH`/`INCR`), `ZREM`, `ZINCRBY`
- `ZRANGE`, `ZREVRANGE`, `ZRANGEBYSCORE`, `ZREVRANGEBYSCORE`
- `ZSCORE`, `ZMSCORE`, `ZRANK`, `ZREVRANK`, `ZCARD`, `ZCOUNT`

*Transactions*
- `MULTI`, `EXEC`, `DISCARD` — queued execution, broadcast on commit

*Pub/Sub*
- `SUBSCRIBE`, `UNSUBSCRIBE`, `PSUBSCRIBE`, `PUNSUBSCRIBE`, `PUBLISH`
- Pattern matching with glob syntax (`*`, `?`, `[...]`)
- Works over both TCP (port 6379) and WebSocket (port 6380)

---

## Roadmap

**New primitives**
- [ ] **Native JSON type** — `JSET`, `JGET`, `JMERGE` with JSONPath; no RedisJSON module
- [ ] **Rate-limiting commands** — `RLSET key limit window` / `RLCHECK key`; replaces hand-rolled Lua scripts
- [ ] **WASM server-side scripting** — run `.wasm` stored procedures instead of Lua; sandboxed, multi-language

**Edge & browser**
- [ ] **WASI target** — `wasm32-wasip1` build for Cloudflare Workers and Deno Deploy
- [ ] **Delta sync** — `SYNC <seq>` protocol; server replays missed mutations from an in-memory ring buffer on WebSocket reconnect; browser uses the WAL sequence number already tracked in `wasm-edge` as the cursor

Intentionally out of scope: RDB/AOF persistence, `REPLICAOF` (the native→browser WebSocket is the sync story), full Redis command parity (250+ commands, Lua scripting, RESP3 — doesn't fit the browser-sync model), server introspection (`INFO`, `SLOWLOG`, `COMMAND`).

---

## Contributing

The most useful contributions right now:

1. **Benchmarks** — `redis-benchmark` against Redis 7 on multi-core hardware (results welcome either way)
2. **Client examples** — React, Vue, or SvelteKit demos using `recached-edge`
3. **Bug reports** — edge cases in the RESP parser, TTL eviction, pub/sub delivery, or WebSocket sync

Open a PR or file an issue.
