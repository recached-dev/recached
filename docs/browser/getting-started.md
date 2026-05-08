# Getting Started (Browser)

The `recached-edge` package is the TypeScript SDK for the browser WASM client. It gives you a `RecachedCache` class backed by the same `core-engine` as the server, with optional WebSocket sync to a Recached server instance.

## Install

```bash
npm install recached-edge
# or
pnpm add recached-edge
# or
yarn add recached-edge
```

---

## Initialize

The WASM module must be initialized once before use. Call `init()` at your app entry point.

```typescript
import init, { RecachedCache } from 'recached-edge'

// Initialize the WASM binary (fetches and compiles recached_edge_bg.wasm)
await init()

// Create a cache instance
const cache = new RecachedCache()
```

For bundlers that support top-level await (Vite, Next.js, modern webpack):

```typescript
// cache.ts — shared singleton
import init, { RecachedCache } from 'recached-edge'

await init()
export const cache = new RecachedCache()
```

---

## Connect to a server

Call `connect()` to enable WebSocket sync. Omit it to use the cache in local-only mode.

```typescript
// Connect to the Recached server WebSocket port
cache.connect('ws://localhost:6380')

// With TLS (production)
cache.connect('wss://cache.yourdomain.com:6380')

// With auth (if RECACHED_PASSWORD is set on the server)
cache.connect('ws://localhost:6380', { password: 'your-secret' })
```

Once connected, any mutation from the server (`SET`, `DEL`, `HSET`, etc.) is automatically pushed to the local WASM store. Any local write is pushed to the server and fanned out to other connected clients.

---

## Basic usage

```typescript
// Strings
cache.set('theme', 'dark')
console.log(cache.get('theme')) // 'dark'

// With expiry (seconds)
cache.set_ex('session:token', 'abc123', 3600)

// Check existence and TTL
cache.exists('theme')      // 1
cache.ttl('session:token') // remaining seconds

// Delete
cache.del('theme')
cache.get('theme')         // null

// Watch a key — fires whenever it changes (from any source)
cache.watch('cart:count', (value) => {
  console.log('Cart count changed to:', value)
})

// Stop watching
cache.unwatch('cart:count')
```

---

## `createCache` with all options

For more control over connection and persistence, use `createCache`:

```typescript
import { createCache } from 'recached-edge'

const cache = await createCache({
  // WebSocket URL (omit for local-only mode)
  url: 'ws://localhost:6380',

  // Server password (if RECACHED_PASSWORD is set)
  password: 'your-secret',

  // Enable IndexedDB persistence (survives page refresh)
  persistence: true,

  // IndexedDB database name (default: 'recached')
  persistenceKey: 'my-app-cache',

  // Reconnect automatically on disconnect (default: true)
  autoReconnect: true,

  // Delay between reconnection attempts in ms (default: 1000)
  reconnectDelay: 1000,

  // Called when the WebSocket connection is established
  onConnect: () => console.log('Connected to Recached server'),

  // Called when the WebSocket connection drops
  onDisconnect: () => console.log('Disconnected'),

  // Called on connection error
  onError: (err) => console.error('Cache connection error:', err),
})
```

---

## React hook example

```typescript
// hooks/useCache.ts
import { useEffect, useState } from 'react'
import { cache } from '../lib/cache' // your shared singleton

export function useCacheValue(key: string): string | null {
  const [value, setValue] = useState<string | null>(() => cache.get(key))

  useEffect(() => {
    // Read the current value immediately
    setValue(cache.get(key))

    // Subscribe to future changes
    cache.watch(key, (newValue) => {
      setValue(newValue)
    })

    return () => {
      cache.unwatch(key)
    }
  }, [key])

  return value
}
```

Usage in a component:

```tsx
// components/CartBadge.tsx
import { useCacheValue } from '../hooks/useCache'

export function CartBadge({ userId }: { userId: number }) {
  const count = useCacheValue(`cart:${userId}:count`)

  return (
    <span className="badge">
      {count ?? '0'}
    </span>
  )
}
```

When the backend calls `SET cart:42:count 3` over RESP, the badge updates automatically — no refetch, no polling, no manual state update.

### With initial data + live updates

```tsx
// components/LiveStock.tsx
import { useEffect, useState } from 'react'
import { cache } from '../lib/cache'

interface StockProps {
  productId: string
  initialStock: number // from SSR / page load
}

export function LiveStock({ productId, initialStock }: StockProps) {
  const key = `stock:${productId}`
  const [stock, setStock] = useState<number>(
    () => {
      const cached = cache.get(key)
      return cached !== null ? parseInt(cached) : initialStock
    }
  )

  useEffect(() => {
    cache.watch(key, (value) => {
      setStock(value !== null ? parseInt(value) : 0)
    })
    return () => cache.unwatch(key)
  }, [key])

  return (
    <span className={stock === 0 ? 'out-of-stock' : 'in-stock'}>
      {stock === 0 ? 'Out of stock' : `${stock} left`}
    </span>
  )
}
```

---

## Svelte example

```svelte
<!-- StockCount.svelte -->
<script lang="ts">
  import { onMount, onDestroy } from 'svelte'
  import { writable } from 'svelte/store'
  import { cache } from '../lib/cache'

  export let productId: string

  const key = `stock:${productId}`
  const stock = writable<string | null>(cache.get(key))

  onMount(() => {
    stock.set(cache.get(key))
    cache.watch(key, (value) => stock.set(value))
  })

  onDestroy(() => {
    cache.unwatch(key)
  })
</script>

{#if $stock === null}
  <span class="loading">—</span>
{:else if $stock === '0'}
  <span class="out-of-stock">Out of stock</span>
{:else}
  <span class="in-stock">{$stock} left</span>
{/if}
```

---

## Without a server (local-only cache)

Do not call `connect()`. The WASM module runs as a pure in-memory cache with TTL — no server, no WebSocket, no backend changes required.

```typescript
import init, { RecachedCache } from 'recached-edge'

await init()
const cache = new RecachedCache()
// No cache.connect() — local-only, no server needed

async function getUser(id: number): Promise<User> {
  const key = `user:${id}`
  const cached = cache.get(key)
  if (cached !== null) return JSON.parse(cached)

  const user: User = await fetch(`/api/users/${id}`).then(r => r.json())
  cache.set_ex(key, JSON.stringify(user), 60) // cache for 60s
  return user
}

async function getProducts(): Promise<Product[]> {
  const cached = cache.get('products')
  if (cached !== null) return JSON.parse(cached)

  const products: Product[] = await fetch('/api/products').then(r => r.json())
  cache.set_ex('products', JSON.stringify(products), 300) // cache for 5 minutes
  return products
}

// Invalidate on mutation
async function updateUserName(id: number, name: string): Promise<void> {
  await fetch(`/api/users/${id}`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name }),
  })
  cache.del(`user:${id}`) // next call to getUser() will refetch
}
```

This pattern replaces the manual `fetchedAt` timestamp approach you might use with Zustand or Redux. TTL is declared once at write time; `get()` returns `null` automatically when the entry has expired.

---

## Bundler configuration

### Vite

Vite handles WASM imports natively. No extra config needed for most setups.

If you see issues with the WASM file not being served, add to `vite.config.ts`:

```typescript
import { defineConfig } from 'vite'

export default defineConfig({
  optimizeDeps: {
    exclude: ['recached-edge'],
  },
})
```

### Next.js (App Router)

```typescript
// app/providers.tsx
'use client'

import { useEffect } from 'react'
import init from 'recached-edge'

export function CacheProvider({ children }: { children: React.ReactNode }) {
  useEffect(() => {
    init().then(() => {
      // WASM ready — connect cache singleton here if needed
    })
  }, [])

  return <>{children}</>
}
```

The `init()` call must happen in a client component (after hydration), not in a server component.

### webpack

Add to your webpack config:

```javascript
module.exports = {
  experiments: {
    asyncWebAssembly: true,
  },
}
```
