# API Reference

Full TypeScript API for the `recached-edge` package.

> **Scope:** The browser SDK exposes a focused set of string-oriented cache operations. Full key-type commands (hashes, lists, sorted sets) are available on the server over RESP (port 6379) from any Redis-compatible client — they are not part of the browser SDK surface.

---

## `init()`

Initializes the WASM module. `createCache()` calls this automatically, so you only need `init()` if you want to pre-load the WASM binary before creating a cache instance.

```typescript
import { init } from 'recached-edge'

await init() // start downloading WASM now
// ... other app setup ...
const cache = await createCache() // resolves immediately — already loaded
```

---

## `createCache(options?)`

Factory function that initializes the WASM module (idempotent), creates a `Cache` instance, and applies the provided options. Returns a fully-ready cache.

```typescript
import { createCache } from 'recached-edge'

async function createCache(options?: CacheOptions): Promise<Cache>
```

### `CacheOptions`

```typescript
interface CacheOptions {
  /** Load the IndexedDB WAL and write-through every mutation. Default: false. */
  persistence?: boolean

  /**
   * BroadcastChannel name. All tabs opened with the same name share mutations
   * automatically, without a server connection.
   */
  broadcastChannel?: string

  /**
   * Connect to a Recached server immediately. Writes are forwarded to the server;
   * server-side mutations are pushed down to the local WASM store.
   */
  connect?: ConnectOptions
}

interface ConnectOptions {
  /** WebSocket URL, e.g. `"ws://localhost:6380"` or `"wss://cache.example.com"`. */
  url: string
  /** Server password. Required when the server has `RECACHED_PASSWORD` set. */
  password?: string
}
```

### Examples

```typescript
// Local-only
const cache = await createCache()

// With persistence (survives page refresh)
const cache = await createCache({ persistence: true })

// With server sync
const cache = await createCache({
  connect: { url: 'ws://localhost:6380' },
})

// With auth
const cache = await createCache({
  connect: { url: 'ws://localhost:6380', password: 'my-secret' },
})

// Full options
const cache = await createCache({
  persistence: true,
  broadcastChannel: 'my-app',
  connect: { url: 'wss://cache.example.com', password: 'secret' },
})
```

---

## `Cache` class

The main cache interface. Obtain instances via `createCache()`.

---

### Reads

#### `get(key)`

Returns the value for a key, or `null` if the key does not exist or has expired.

```typescript
get(key: string): string | null
```

```typescript
cache.set('name', 'Alice')
cache.get('name')     // 'Alice'
cache.get('missing')  // null
```

#### `getJSON<T>(key)`

Returns a JSON-parsed value, or `null` if the key is missing, expired, or not valid JSON.

```typescript
getJSON<T>(key: string): T | null
```

```typescript
interface User { id: number; name: string }
const user = cache.getJSON<User>('user:42') // User | null
```

#### `exists(key)`

Returns `true` if the key exists and has not expired.

```typescript
exists(key: string): boolean
```

#### `ttl(key)`

Returns the remaining TTL in seconds. Returns `-1` if the key has no expiry, `-2` if the key does not exist.

```typescript
ttl(key: string): number
```

---

### Writes

All write methods also notify `onMutation` listeners and, when connected, forward the change to the server and other tabs via BroadcastChannel.

#### `set(key, value)`

Sets a key to a string value. Overwrites any existing value and removes any existing TTL.

```typescript
set(key: string, value: string): void
```

#### `setEx(key, value, seconds)`

Sets a key with a TTL in seconds. The key is deleted automatically when the TTL elapses.

```typescript
setEx(key: string, value: string, seconds: number): void
```

```typescript
cache.setEx('session', JSON.stringify({ userId: 1 }), 3600)
```

#### `setJSON<T>(key, value, ttl?)`

Serializes `value` as JSON and stores it. Pass `ttl` (seconds) to set an expiry.

```typescript
setJSON<T>(key: string, value: T, ttl?: number): void
```

```typescript
cache.setJSON('user:42', { id: 42, name: 'Alice' })
cache.setJSON('session', { userId: 1 }, 3600) // expires in 1h
```

#### `del(key)`

Deletes a key. Returns `true` if the key existed, `false` if it did not.

```typescript
del(key: string): boolean
```

---

### Reactivity

#### `onMutation(callback)`

Registers a callback that fires whenever the local store changes from any source — local writes, server pushes, or BroadcastChannel cross-tab messages.

Returns an unsubscribe function. Pass it directly to React's `useSyncExternalStore` or call it in a cleanup function.

```typescript
onMutation(cb: () => void): () => void
```

```typescript
// Manual wiring
const unsubscribe = cache.onMutation(() => {
  const count = cache.get('cart:count')
  document.getElementById('badge')!.textContent = count ?? '0'
})

// React (useSyncExternalStore)
const count = useSyncExternalStore(
  (cb) => cache.onMutation(cb),
  () => cache.get('cart:count'),
  () => null,
)

// Cleanup
unsubscribe()
```

The callback receives no arguments — it signals that _something_ changed. Read the keys you care about inside the callback.

---

### Pub/Sub

Pub/Sub requires a server connection. Messages are delivered via the WebSocket and routed to subscribers on the receiving end.

#### `subscribe(channel)`

Subscribe to a pub/sub channel. Incoming messages from the server are applied to the local store.

```typescript
subscribe(channel: string): void
```

#### `unsubscribe(channel)`

Unsubscribe from a pub/sub channel.

```typescript
unsubscribe(channel: string): void
```

#### `publish(channel, message)`

Publish a message to a pub/sub channel. All server-side and browser-side subscribers receive it.

```typescript
publish(channel: string, message: string): void
```

---

### Persistence

#### `clearPersistence()`

Deletes the IndexedDB WAL database. Use on sign-out so the next session starts clean.

```typescript
clearPersistence(): Promise<void>
```

```typescript
async function signOut() {
  await cache.clearPersistence()
  window.location.href = '/login'
}
```

Persistence is enabled via `createCache({ persistence: true })`, not on the `Cache` instance itself.

---

### Escape hatch

#### `cache.raw`

Direct access to the underlying WASM instance (`RecachedCache` from wasm-bindgen). Use this when you need an operation that is not yet exposed in the TypeScript wrapper.

```typescript
get raw(): RawCache
```

Available methods on `raw`: `set()`, `set_ex()`, `get()`, `del()`, `ttl()`, `exists()`, `subscribe()`, `unsubscribe()`, `publish()`, `connect()`, `auth()`, `broadcast()`, `enable_persistence()`, `clear_persistence()`, `set_mutation_callback()`, `free()`.

> Writes through `cache.raw` bypass the `onMutation` notification bus. Use the typed `Cache` methods when possible.
