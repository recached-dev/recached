# API Reference

Full TypeScript API for the `recached-edge` package.

## `init()`

Initializes the WASM module. Must be called once before creating any `RecachedCache` instances. Returns a promise that resolves when the WASM binary is loaded and compiled.

```typescript
function init(input?: RequestInfo | URL | Response | BufferSource | WebAssembly.Module): Promise<void>
```

```typescript
import init from 'recached-edge'
await init()
```

If you are serving the WASM file from a custom URL (CDN, different path):

```typescript
await init('https://cdn.example.com/recached_edge_bg.wasm')
```

---

## `createCache(options)`

Factory function that initializes the WASM module (if not already initialized), creates a `RecachedCache` instance, optionally connects to a server, and optionally enables persistence. Returns a fully-ready cache instance.

```typescript
interface CreateCacheOptions {
  /** WebSocket URL of the Recached server (port 6380). Omit for local-only mode. */
  url?: string

  /** Password for AUTH. Required if RECACHED_PASSWORD is set on the server. */
  password?: string

  /** Enable IndexedDB WAL persistence. Cache survives page refresh. Default: false. */
  persistence?: boolean

  /** IndexedDB database name. Default: 'recached'. */
  persistenceKey?: string

  /** Reconnect automatically on WebSocket disconnect. Default: true. */
  autoReconnect?: boolean

  /** Delay in milliseconds between reconnection attempts. Default: 1000. */
  reconnectDelay?: number

  /** Called when the WebSocket connection is established. */
  onConnect?: () => void

  /** Called when the WebSocket connection drops. */
  onDisconnect?: () => void

  /** Called on WebSocket error. */
  onError?: (error: Event) => void
}

async function createCache(options?: CreateCacheOptions): Promise<Cache>
```

```typescript
import { createCache } from 'recached-edge'

const cache = await createCache({
  url: 'ws://localhost:6380',
  password: 'my-secret',
  persistence: true,
  onConnect: () => console.log('Connected'),
  onDisconnect: () => console.warn('Disconnected from cache server'),
})
```

---

## `RecachedCache` class

The main cache class. Wraps the WASM `core-engine` instance and the optional WebSocket connection.

### Constructor

```typescript
new RecachedCache(): RecachedCache
```

Creates a new cache instance in local-only mode. Call `connect()` to enable server sync. Prefer `createCache()` for a higher-level setup.

---

### Connection

#### `connect(url, options?)`

Connect to a Recached server over WebSocket. Mutations from the server are applied to the local store automatically. Local writes are forwarded to the server.

```typescript
connect(url: string, options?: { password?: string }): void
```

```typescript
cache.connect('ws://localhost:6380')
cache.connect('wss://cache.example.com:6380', { password: 'secret' })
```

#### `disconnect()`

Close the WebSocket connection. The cache continues to work in local-only mode.

```typescript
disconnect(): void
```

#### `isConnected()`

Returns `true` if the WebSocket is currently open.

```typescript
isConnected(): boolean
```

---

### String commands

#### `get(key)`

Returns the value for a key, or `null` if the key does not exist or has expired.

```typescript
get(key: string): string | null
```

```typescript
cache.set('name', 'Alice')
cache.get('name')  // 'Alice'
cache.get('missing')  // null
```

#### `set(key, value)`

Sets a key to a string value. If the key already exists, its value is overwritten and any existing TTL is removed.

```typescript
set(key: string, value: string): void
```

#### `set_ex(key, value, seconds)`

Sets a key with an expiry in seconds. The key is automatically deleted after `seconds` seconds.

```typescript
set_ex(key: string, value: string, seconds: number): void
```

```typescript
cache.set_ex('session', JSON.stringify({ userId: 1 }), 3600)
```

#### `set_px(key, value, milliseconds)`

Sets a key with an expiry in milliseconds.

```typescript
set_px(key: string, value: string, milliseconds: number): void
```

#### `set_nx(key, value)`

Sets a key only if it does not already exist. Returns `true` if the key was set, `false` if it already existed.

```typescript
set_nx(key: string, value: string): boolean
```

#### `getset(key, value)`

Sets a key to a new value and returns the old value, or `null` if the key did not exist.

```typescript
getset(key: string, value: string): string | null
```

#### `mget(...keys)`

Returns the values of multiple keys as an array. Non-existent or expired keys return `null` at the corresponding index.

```typescript
mget(...keys: string[]): (string | null)[]
```

```typescript
cache.mset('a', '1', 'b', '2')
cache.mget('a', 'b', 'c')  // ['1', '2', null]
```

#### `mset(...keyValues)`

Sets multiple key-value pairs in a single operation.

```typescript
mset(...keyValues: string[]): void
```

```typescript
cache.mset('a', '1', 'b', '2', 'c', '3')
```

#### `append(key, value)`

Appends a string to the existing value. Creates the key if it does not exist. Returns the new length.

```typescript
append(key: string, value: string): number
```

#### `strlen(key)`

Returns the length of the string stored at key, or 0 if the key does not exist.

```typescript
strlen(key: string): number
```

#### `incr(key)`

Increments the integer value of a key by 1. Creates the key with value 1 if it does not exist. Returns the new value.

```typescript
incr(key: string): number
```

#### `decr(key)`

Decrements the integer value of a key by 1.

```typescript
decr(key: string): number
```

#### `incrby(key, increment)`

Increments the integer value of a key by a given integer.

```typescript
incrby(key: string, increment: number): number
```

#### `decrby(key, decrement)`

Decrements the integer value of a key by a given integer.

```typescript
decrby(key: string, decrement: number): number
```

---

### Key management

#### `del(...keys)`

Deletes one or more keys. Returns the number of keys actually deleted.

```typescript
del(...keys: string[]): number
```

#### `exists(...keys)`

Returns the number of keys that exist among the arguments. A key listed multiple times counts multiple times.

```typescript
exists(...keys: string[]): number
```

#### `expire(key, seconds)`

Sets a TTL on a key in seconds. Returns `true` if the TTL was set, `false` if the key does not exist.

```typescript
expire(key: string, seconds: number): boolean
```

#### `pexpire(key, milliseconds)`

Sets a TTL in milliseconds.

```typescript
pexpire(key: string, milliseconds: number): boolean
```

#### `ttl(key)`

Returns the remaining TTL in seconds. Returns `-2` if the key does not exist, `-1` if the key has no expiry.

```typescript
ttl(key: string): number
```

#### `pttl(key)`

Returns the remaining TTL in milliseconds.

```typescript
pttl(key: string): number
```

#### `persist(key)`

Removes the TTL from a key, making it persistent. Returns `true` if the TTL was removed.

```typescript
persist(key: string): boolean
```

#### `type(key)`

Returns the type of the value stored at key: `'string'`, `'hash'`, `'list'`, `'set'`, `'zset'`, or `'none'`.

```typescript
type(key: string): 'string' | 'hash' | 'list' | 'set' | 'zset' | 'none'
```

#### `rename(key, newKey)`

Renames a key. Throws if the source key does not exist.

```typescript
rename(key: string, newKey: string): void
```

#### `keys(pattern)`

Returns all keys matching the glob pattern. `*` matches any sequence, `?` matches a single character.

```typescript
keys(pattern: string): string[]
```

#### `dbsize()`

Returns the total number of keys in the local store.

```typescript
dbsize(): number
```

#### `flushdb()`

Removes all keys from the local store. If connected to a server, also flushes the server.

```typescript
flushdb(): void
```

---

### Hash commands

#### `hset(key, ...fieldValues)`

Sets one or more fields in a hash. Returns the number of new fields added.

```typescript
hset(key: string, ...fieldValues: string[]): number
```

```typescript
cache.hset('user:1', 'name', 'Alice', 'plan', 'pro', 'credits', '500')
```

#### `hget(key, field)`

Returns the value of a field in a hash, or `null` if the field or hash does not exist.

```typescript
hget(key: string, field: string): string | null
```

#### `hgetall(key)`

Returns all field-value pairs of a hash as a plain object. Returns `null` if the hash does not exist.

```typescript
hgetall(key: string): Record<string, string> | null
```

```typescript
cache.hgetall('user:1')
// { name: 'Alice', plan: 'pro', credits: '500' }
```

#### `hdel(key, ...fields)`

Deletes one or more fields from a hash. Returns the number of fields removed.

```typescript
hdel(key: string, ...fields: string[]): number
```

#### `hmget(key, ...fields)`

Returns the values of multiple fields. Non-existent fields return `null`.

```typescript
hmget(key: string, ...fields: string[]): (string | null)[]
```

#### `hkeys(key)`

Returns all field names in the hash.

```typescript
hkeys(key: string): string[]
```

#### `hvals(key)`

Returns all values in the hash.

```typescript
hvals(key: string): string[]
```

#### `hlen(key)`

Returns the number of fields in the hash.

```typescript
hlen(key: string): number
```

#### `hexists(key, field)`

Returns `true` if the field exists in the hash.

```typescript
hexists(key: string, field: string): boolean
```

#### `hincrby(key, field, increment)`

Increments the integer value of a hash field. Returns the new value.

```typescript
hincrby(key: string, field: string, increment: number): number
```

#### `hincrbyfloat(key, field, increment)`

Increments the float value of a hash field. Returns the new value as a string.

```typescript
hincrbyfloat(key: string, field: string, increment: number): string
```

---

### List commands

#### `lpush(key, ...elements)` / `rpush(key, ...elements)`

Push elements to the head or tail of a list. Returns the new list length.

```typescript
lpush(key: string, ...elements: string[]): number
rpush(key: string, ...elements: string[]): number
```

#### `lpop(key, count?)` / `rpop(key, count?)`

Remove and return elements from the head or tail of a list. With no `count`, returns a single string or `null`. With `count`, returns an array.

```typescript
lpop(key: string): string | null
lpop(key: string, count: number): string[]
rpop(key: string): string | null
rpop(key: string, count: number): string[]
```

#### `lrange(key, start, stop)`

Returns elements between indices `start` and `stop` (inclusive). Negative indices count from the tail.

```typescript
lrange(key: string, start: number, stop: number): string[]
```

```typescript
cache.rpush('queue', 'a', 'b', 'c')
cache.lrange('queue', 0, -1)  // ['a', 'b', 'c']
```

#### `llen(key)`

Returns the length of the list.

```typescript
llen(key: string): number
```

#### `lindex(key, index)`

Returns the element at the given index.

```typescript
lindex(key: string, index: number): string | null
```

#### `lset(key, index, element)`

Sets the element at the given index.

```typescript
lset(key: string, index: number, element: string): void
```

#### `lrem(key, count, element)`

Removes occurrences of an element.

```typescript
lrem(key: string, count: number, element: string): number
```

#### `ltrim(key, start, stop)`

Trims the list to the elements between `start` and `stop`.

```typescript
ltrim(key: string, start: number, stop: number): void
```

---

### Set commands

#### `sadd(key, ...members)` / `srem(key, ...members)`

Add or remove members. Return the number of members added or removed.

```typescript
sadd(key: string, ...members: string[]): number
srem(key: string, ...members: string[]): number
```

#### `smembers(key)`

Returns all members of the set as an array.

```typescript
smembers(key: string): string[]
```

#### `scard(key)`

Returns the number of members.

```typescript
scard(key: string): number
```

#### `sismember(key, member)`

Returns `true` if the member exists in the set.

```typescript
sismember(key: string, member: string): boolean
```

#### `sinter(...keys)` / `sunion(...keys)` / `sdiff(...keys)`

Returns the intersection, union, or difference of the given sets.

```typescript
sinter(...keys: string[]): string[]
sunion(...keys: string[]): string[]
sdiff(...keys: string[]): string[]
```

---

### Sorted Set commands

#### `zadd(key, score, member)` / `zadd(key, options, score, member, ...)`

Add a member with a score.

```typescript
zadd(key: string, score: number, member: string): number
```

#### `zrange(key, start, stop, options?)`

Returns members between ranks `start` and `stop`.

```typescript
zrange(key: string, start: number, stop: number, options?: { withScores?: boolean }): string[]
```

#### `zscore(key, member)`

Returns the score of a member, or `null` if the member does not exist.

```typescript
zscore(key: string, member: string): string | null
```

#### `zrank(key, member)` / `zrevrank(key, member)`

Returns the rank (ascending or descending) of a member.

```typescript
zrank(key: string, member: string): number | null
zrevrank(key: string, member: string): number | null
```

#### `zcard(key)` / `zcount(key, min, max)`

```typescript
zcard(key: string): number
zcount(key: string, min: number | '-inf', max: number | '+inf'): number
```

---

### Pub/Sub

#### `subscribe(channel, callback)`

Subscribe to a pub/sub channel. The callback is called with the message whenever something is published to the channel.

```typescript
subscribe(channel: string, callback: (message: string) => void): void
```

#### `unsubscribe(channel?)`

Unsubscribe from a channel. With no argument, unsubscribes from all channels.

```typescript
unsubscribe(channel?: string): void
```

#### `publish(channel, message)`

Publish a message to a channel. Returns the number of clients that received the message.

```typescript
publish(channel: string, message: string): number
```

---

### Observable keys

#### `watch(key, callback)`

Register a callback that fires whenever the given key changes. The callback receives the new value as a string, or `null` if the key was deleted.

The callback fires for changes from any source: local writes, server pushes, or other tabs via BroadcastChannel.

```typescript
watch(key: string, callback: (value: string | null) => void): void
```

```typescript
cache.watch('cart:42:count', (value) => {
  document.getElementById('cart-count')!.textContent = value ?? '0'
})
```

#### `unwatch(key?)`

Stop watching a key. With no argument, clears all watches.

```typescript
unwatch(key?: string): void
```

---

### Persistence

#### `enable_persistence(dbName?)`

Enable IndexedDB WAL persistence. After this call, every write is appended to an IndexedDB WAL. On the next page load, the WAL is replayed before connecting to the server.

```typescript
enable_persistence(dbName?: string): Promise<void>
```

```typescript
await cache.enable_persistence('my-app-cache')
```

#### `clearPersistence(dbName?)`

Delete the IndexedDB WAL database. Call this on sign-out.

```typescript
clearPersistence(dbName?: string): Promise<void>
```

---

### Escape hatch

#### `cache.raw`

Direct access to the underlying WASM `RecachedCache` instance. Use when you need a command that is not yet exposed in the TypeScript wrapper.

```typescript
get raw(): RecachedCache
```

```typescript
// Execute a raw RESP command string
const result = cache.raw.exec_raw('*3\r\n$3\r\nSET\r\n$3\r\nfoo\r\n$3\r\nbar\r\n')
```

Note: writes through `cache.raw` bypass the BroadcastChannel and persistence layers. Use the typed methods when possible.
