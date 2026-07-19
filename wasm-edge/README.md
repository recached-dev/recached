# recached-edge

The browser and edge runtime client for [Recached](https://github.com/thinkgrid-labs/recached) — a Rust-powered in-memory cache that runs natively on the server and inside the browser via WebAssembly.

Zero-latency local reads. Automatic background sync to the Recached server over WebSockets.

## Install

```bash
npm install recached-edge
```

## Quick start

```typescript
import { createCache } from 'recached-edge';

// Loads and initialises the WASM module automatically
const cache = await createCache({
  persistence: true,                              // survive page refresh via IndexedDB
  connect: { url: 'ws://localhost:6380' },        // optional — local reads work without it
});

cache.set('user:theme', 'dark');                 // local + server + IndexedDB
cache.get('user:theme');                         // "dark" — from local WASM memory, 0 ms
cache.del('user:theme');                         // boolean: true if the key existed
```

### Raw WASM API (advanced)

If you need direct access to the wasm-bindgen bindings, the raw module is still available:

```javascript
import init, { RecachedCache } from 'recached-edge/pkg/recached_edge.js';

await init();
const raw = new RecachedCache();
raw.connect('ws://localhost:6380');
raw.set('key', 'value');
```

## How sync works

```
Browser (recached-edge)          Recached Server
       │                                │
       │  SET user:theme dark  ────────►│  stores in server
       │                       ◄────────│  broadcasts to other clients
       │                                │
other  │◄── SET user:theme dark ────────│  other browser tabs update automatically
tabs   │                                │
```

Any `set` or `del` in the browser is pushed to the server and fanned out to all other connected clients. Any mutation on the server is pushed down to all connected browsers. Reads always come from local WASM memory — no network hop.

## API

### `createCache(options?): Promise<Cache>`

Creates a `Cache` instance. Loads and initialises the WASM module on first call; subsequent calls reuse the existing module. All options are applied atomically during construction.

| Option | Type | Default | Description |
|---|---|---|---|
| `persistence` | `boolean` | `false` | Hydrate from IndexedDB WAL and enable write-through persistence |
| `broadcastChannel` | `string` | — | BroadcastChannel name for cross-tab sync |
| `connect.url` | `string` | — | WebSocket URL of the Recached server |
| `connect.password` | `string` | — | Server password (sent via `AUTH` on connect) |

```typescript
import { createCache } from 'recached-edge';

const cache = await createCache({
  persistence: true,
  broadcastChannel: 'app-cache',
  connect: { url: 'wss://cache.example.com', password: 'secret' },
});
```

---

### `init(): Promise<void>`

Eagerly load and initialise the WASM module. Call during a loading screen to ensure `createCache` resolves instantly when the user first interacts.

```typescript
import { init, createCache } from 'recached-edge';

await init(); // download WASM now
// ... other setup ...
const cache = await createCache(); // instant — already loaded
```

---

### `cache.get(key): string | null`

Return the value for `key`, or `null` if missing or expired. Always local — zero network.

---

### `cache.getJSON<T>(key): T | null`

Parse a JSON value stored under `key`. Returns `null` if missing or invalid JSON.

```typescript
interface User { id: number; name: string }
const user = cache.getJSON<User>('user:42'); // User | null
```

---

### `cache.set(key, value): void`

Store a string value. Syncs to the server and other tabs when connected.

---

### `cache.setEx(key, value, seconds): void`

Store a string value with a TTL. The key is deleted automatically when the TTL elapses.

---

### `cache.setJSON<T>(key, value, ttl?): void`

Serialize `value` as JSON and store it under `key`. Pass `ttl` (seconds) to set an expiry.

```typescript
cache.setJSON('user:42', { id: 42, name: 'Alice' }, 300); // expires in 5 min
```

---

### `cache.del(key): boolean`

Delete `key`. Returns `true` if the key existed, `false` if not.

---

### `cache.exists(key): boolean` / `cache.ttl(key): number`

Check key state. `ttl` returns `-1` (no expiry) or `-2` (not found).

---

### `cache.subscribe(channel)` / `cache.unsubscribe(channel)` / `cache.publish(channel, message)`

Server pub/sub. Works over the same WebSocket connection as cache sync.

---

### `cache.clearPersistence(): Promise<void>`

Erase the IndexedDB WAL. The in-memory store is not affected.

---

### `cache.raw`

The underlying wasm-bindgen `RecachedCache` instance for advanced use cases.

---

### `cache.enable_persistence(): Promise<void>` *(raw API)*

Opens the IndexedDB WAL, replays all stored commands into the in-memory store, and enables persistence for future writes. Call once at startup, before any reads or writes, if you want the cache to survive page refreshes.

```javascript
await init();
const cache = new RecachedCache();
await cache.enable_persistence(); // hydrate from IndexedDB

// Keys from the previous session are now available
const theme = cache.get('user:theme'); // "dark" — loaded from IDB, zero network
```

Works with or without a server connection. If you later call `connect()`, the server will push current state and overwrite any stale keys.

---

### `cache.clear_persistence(): Promise<void>`

Erases the IndexedDB WAL. The in-memory store is not affected. Useful for sign-out flows.

```javascript
await cache.clear_persistence();
```

---

### `cache.connect(url: string): void`

Connects to a Recached server over WebSocket and begins syncing state. Calling `connect()` again on an existing instance cleanly replaces the previous connection without leaking memory.

```javascript
cache.connect('ws://localhost:6380');

// With a custom domain
cache.connect('wss://cache.example.com');
```

---

### `cache.auth(password: string): void`

Sends an `AUTH` command to the server. Call this immediately after `connect()` if the server has `RECACHED_PASSWORD` set. The result is delivered asynchronously via the WebSocket.

```javascript
cache.connect('ws://localhost:6380');
cache.auth('my-secret-password');
```

---

### `cache.set(key: string, value: string): string`

Stores a key-value pair in local memory and syncs it to the server. Returns `"OK"` on success or an error string if the server's key limit is reached.

```javascript
cache.set('session:abc', JSON.stringify({ userId: 42 }));
```

---

### `cache.get(key: string): string | undefined`

Returns the value for a key from local memory, or `undefined` if the key does not exist. Always reads locally — no network latency.

```javascript
const raw = cache.get('session:abc');
const session = raw ? JSON.parse(raw) : null;
```

---

### `cache.del(key: string): number`

Deletes a key from local memory and syncs the deletion to the server. Returns `1` if the key existed, `0` if it did not.

```javascript
cache.del('session:abc'); // 1
```

---

## Framework examples

### React

```typescript
import { useEffect, useState } from 'react';
import { createCache, type Cache } from 'recached-edge';

let _cache: Cache | null = null;

export function useRecached() {
  const [cache, setCache] = useState<Cache | null>(null);

  useEffect(() => {
    if (_cache) { setCache(_cache); return; }
    createCache({
      persistence: true,
      connect: { url: 'ws://localhost:6380' },
    }).then((c) => { _cache = c; setCache(c); });
  }, []);

  return cache;
}
```

```typescript
// Usage in a component
function ThemeToggle() {
  const cache = useRecached();
  const toggle = () => cache?.set('theme', theme === 'dark' ? 'light' : 'dark');
  // ...
}
```

### Without a server (browser-only local cache)

`recached-edge` works standalone as a fast in-process key-value store with no server required. Just omit `connect`.

```typescript
import { createCache } from 'recached-edge';

const cache = await createCache();

cache.set('theme', 'dark');
cache.get('theme');           // "dark" — fully local, no network
cache.setJSON('prefs', { fontSize: 16 }, 3600); // typed, expires in 1 hour
cache.getJSON<{ fontSize: number }>('prefs');    // { fontSize: 16 } | null
```

### Offline-first — cache that survives page refresh

Call `enable_persistence()` once at startup. The cache replays its IndexedDB WAL before your first read, so users never see a blank state on refresh.

```javascript
await init();
const cache = new RecachedCache();
await cache.enable_persistence(); // load previous session from IndexedDB

cache.set('theme', 'dark');       // written to memory + queued to IndexedDB
cache.get('theme');               // "dark"

// --- page refresh ---

await init();
const cache = new RecachedCache();
await cache.enable_persistence(); // replays WAL — "theme" is back immediately
cache.get('theme');               // "dark" — zero network, zero flicker
```

Works with or without a server. If you later call `connect()`, the server pushes current state and overwrites any stale keys. To drop all persisted state on sign-out:

```javascript
await cache.clear_persistence();
```

---

## Running the server

```bash
# Docker
docker run -p 6380:6380 ghcr.io/thinkgrid-labs/recached:latest

# With authentication
docker run -p 6380:6380 -e RECACHED_PASSWORD=secret ghcr.io/thinkgrid-labs/recached:latest
```

See the [Recached README](https://github.com/thinkgrid-labs/recached) for full server configuration.

## Browser compatibility

Requires WebAssembly support (all modern browsers) and the WebSocket API for server sync. Works in Cloudflare Workers and Deno Deploy with the WASI build target (coming soon).

## License

Apache-2.0
