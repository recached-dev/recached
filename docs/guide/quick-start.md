# Quick Start

This guide walks you from zero to a working Recached setup: server running, backend connected, browser WASM client syncing live.

## 1. Run the server

Pick the method that fits your environment.

### Docker (recommended)

```bash
docker run -p 6379:6379 -p 6380:6380 ghcr.io/thinkgrid-labs/recached:latest
```

Port 6379 is the RESP TCP port (Redis-compatible). Port 6380 is the WebSocket sync port for browser clients.

### Homebrew (macOS)

```bash
brew tap thinkgrid-labs/recached
brew install recached
recached-server
```

### Cargo

```bash
cargo install recached
recached-server
```

### Verify the server is running

```bash
redis-cli ping
# PONG
```

Any Redis CLI tool works. The server speaks RESP.

---

## 2. Connect from your backend

Use any Redis client. No special driver needed — Recached speaks RESP on port 6379.

### Node.js (ioredis)

```typescript
import Redis from 'ioredis'

const cache = new Redis('redis://127.0.0.1:6379')

// Strings
await cache.set('user:1:name', 'Alice')
console.log(await cache.get('user:1:name')) // 'Alice'

// With expiry
await cache.setex('session:abc123', 3600, JSON.stringify({ userId: 1, role: 'admin' }))

// Hash (structured objects)
await cache.hset('user:1:profile', 'name', 'Alice', 'plan', 'pro', 'credits', '500')
const profile = await cache.hgetall('user:1:profile')
// { name: 'Alice', plan: 'pro', credits: '500' }

// Counter
await cache.set('views:post:42', '0')
await cache.incr('views:post:42')
await cache.incr('views:post:42')
console.log(await cache.get('views:post:42')) // '2'

// List
await cache.rpush('queue:jobs', 'task-a', 'task-b', 'task-c')
console.log(await cache.lpop('queue:jobs')) // 'task-a'

// Pub/Sub
const publisher = new Redis('redis://127.0.0.1:6379')
const subscriber = new Redis('redis://127.0.0.1:6379')

await subscriber.subscribe('notifications')
subscriber.on('message', (channel, message) => {
  console.log(`[${channel}] ${message}`)
})

await publisher.publish('notifications', 'New order received')
// [notifications] New order received
```

### Python (redis-py)

```python
import redis

r = redis.Redis(host='127.0.0.1', port=6379)

r.set('product:99:stock', 42)
r.decr('product:99:stock')

print(r.get('product:99:stock'))  # b'41'
print(r.ttl('product:99:stock'))  # -1 (no expiry)

r.setex('rate:ip:192.168.1.1', 60, 1)  # expires in 60s
```

---

## 3. Connect the browser WASM client

Install the package:

```bash
npm install recached-edge
```

Then connect to the WebSocket port:

```typescript
import init, { RecachedCache } from 'recached-edge'

// Initialize the WASM module (call once at app startup)
await init()

const cache = new RecachedCache()

// Connect to the server WebSocket port
// All mutations from the server are pushed here automatically
cache.connect('ws://127.0.0.1:6380')

// Reads are local — 0 ms, no network
cache.set('theme', 'dark')
console.log(cache.get('theme')) // 'dark'

// Set with expiry (seconds)
cache.set_ex('api:response:products', JSON.stringify(products), 300)
```

---

## 4. Live sync example

This is what makes Recached different from a regular cache. Write on the server; the browser sees the update instantly.

### Backend (Node.js)

```typescript
import Redis from 'ioredis'
import express from 'express'

const cache = new Redis('redis://127.0.0.1:6379')
const app = express()

app.post('/api/cart/add', express.json(), async (req, res) => {
  const { userId, itemId } = req.body

  // Add to the user's cart set
  await cache.sadd(`cart:${userId}`, itemId)

  // Update the cart count (a separate key for fast reads)
  const count = await cache.scard(`cart:${userId}`)
  await cache.set(`cart:${userId}:count`, String(count))

  res.json({ ok: true, count })
})

app.listen(3000)
```

### Browser (TypeScript)

```typescript
import init, { RecachedCache } from 'recached-edge'

await init()
const cache = new RecachedCache()
cache.connect('ws://localhost:6380')

const userId = 42

// Watch the cart count key — called automatically whenever it changes on the server
cache.watch(`cart:${userId}:count`, (newValue) => {
  const count = newValue ? parseInt(newValue) : 0
  document.getElementById('cart-badge')!.textContent = String(count)
})

// Initial read (from local WASM memory — 0 ms if already synced)
const initialCount = cache.get(`cart:${userId}:count`)
if (initialCount !== null) {
  document.getElementById('cart-badge')!.textContent = initialCount
}
```

When your backend calls `cache.set('cart:42:count', '3')` over RESP, the browser's `watch` callback fires with `'3'` — no polling, no extra endpoint, no client-side invalidation code.

---

## 5. Local-only (no server)

The WASM module works entirely without a server. Never call `connect()` and the cache is a local in-memory store with built-in TTL.

```typescript
import init, { RecachedCache } from 'recached-edge'

await init()
const cache = new RecachedCache()
// No cache.connect() — purely local, no server, no WebSocket

async function getProducts(): Promise<Product[]> {
  const cached = cache.get('products')
  if (cached !== null) {
    return JSON.parse(cached)
  }

  const data: Product[] = await fetch('/api/products').then(r => r.json())

  // Cache for 5 minutes
  cache.set_ex('products', JSON.stringify(data), 300)

  return data
}

async function getUser(id: number): Promise<User> {
  const key = `user:${id}`
  const cached = cache.get(key)
  if (cached !== null) return JSON.parse(cached)

  const user: User = await fetch(`/api/users/${id}`).then(r => r.json())
  cache.set_ex(key, JSON.stringify(user), 60)
  return user
}

// Manual invalidation after a mutation
async function updateUser(id: number, patch: Partial<User>) {
  await fetch(`/api/users/${id}`, {
    method: 'PATCH',
    body: JSON.stringify(patch),
    headers: { 'Content-Type': 'application/json' },
  })
  cache.del(`user:${id}`) // next read will refetch
}
```

No Zustand slice. No `fetchedAt` timestamp check. TTL is declared once at write time.

---

## Next steps

- [How It Works](/guide/how-it-works) — understand the sync protocol, IndexedDB persistence, and cross-tab sync
- [Server Configuration](/server/configuration) — TLS, auth, metrics, eviction policies
- [Browser API Reference](/browser/api-reference) — full TypeScript API for `RecachedCache`
