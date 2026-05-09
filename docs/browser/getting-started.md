# Getting Started (Browser)

The `recached-edge` package is the TypeScript SDK for the browser WASM client. It gives you a `Cache` class backed by the same `core-engine` as the server, with optional WebSocket sync to a Recached server instance.

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

Use `createCache()` — it initializes the WASM module and returns a ready `Cache` instance.

```typescript
import { createCache } from 'recached-edge'

// Local-only, in-memory cache (no server connection)
const cache = await createCache()
```

For bundlers that support top-level await (Vite, Next.js, modern webpack):

```typescript
// lib/cache.ts — shared singleton
import { createCache } from 'recached-edge'

export const cache = await createCache()
```

---

## Connect to a server

Pass `connect` to `createCache()` to enable WebSocket sync. Omit it for local-only mode.

```typescript
import { createCache } from 'recached-edge'

// Connect to the Recached server WebSocket port
const cache = await createCache({
  connect: { url: 'ws://localhost:6380' },
})

// With TLS (production)
const cache = await createCache({
  connect: { url: 'wss://cache.yourdomain.com:6380' },
})

// With auth (if RECACHED_PASSWORD is set on the server)
const cache = await createCache({
  connect: { url: 'ws://localhost:6380', password: 'your-secret' },
})
```

Once connected, any mutation from the server (`SET`, `DEL`, etc.) is automatically pushed to the local WASM store. Any local write is forwarded to the server and fanned out to other connected clients.

---

## Basic usage

```typescript
// Strings
cache.set('theme', 'dark')
console.log(cache.get('theme')) // 'dark'

// With expiry (seconds)
cache.setEx('session:token', 'abc123', 3600)

// Check existence and TTL
cache.exists('theme')       // true
cache.ttl('session:token')  // remaining seconds

// Delete
cache.del('theme')
cache.get('theme')          // null

// React to any mutation (from any source — local, server, or other tabs)
const unsubscribe = cache.onMutation(() => {
  const count = cache.get('cart:count')
  console.log('Cart count is now:', count)
})

// Stop listening
unsubscribe()
```

---

## `createCache` options

```typescript
import { createCache } from 'recached-edge'

const cache = await createCache({
  // Enable IndexedDB persistence (survives page refresh)
  persistence: true,

  // BroadcastChannel name for cross-tab mutation sharing
  broadcastChannel: 'my-app-cache',

  // Connect to the Recached server WebSocket port
  connect: {
    url: 'ws://localhost:6380',
    // Server password (if RECACHED_PASSWORD is set)
    password: 'your-secret',
  },
})
```

All three options are independent — you can use persistence and cross-tab sync without a server connection.

---

## React

If you are using React, install the official hooks package instead:

```bash
npm install @recached/react
```

```tsx
import { RecachedProvider, useKey } from '@recached/react'

function App() {
  return (
    <RecachedProvider options={{ connect: { url: 'ws://localhost:6380' } }}>
      <CartBadge userId={42} />
    </RecachedProvider>
  )
}

function CartBadge({ userId }: { userId: number }) {
  const count = useKey(`cart:${userId}:count`)
  return <span className="badge">{count ?? '0'}</span>
}
```

See the [React hooks docs](/react/getting-started) for the full guide.

---

## Vue

If you are using Vue 3, install the official composables package instead:

```bash
npm install @recached/vue
```

```ts
// main.ts
import { createApp } from 'vue'
import { RecachedPlugin } from '@recached/vue'
import App from './App.vue'

const app = createApp(App)
app.use(RecachedPlugin, { connect: { url: 'ws://localhost:6380' } })
app.mount('#app')
```

```vue
<!-- CartBadge.vue -->
<script setup lang="ts">
import { useKey } from '@recached/vue'
const props = defineProps<{ userId: number }>()
const count = useKey(`cart:${props.userId}:count`)
</script>

<template>
  <span class="badge">{{ count ?? '0' }}</span>
</template>
```

See the [Vue composables docs](/vue/getting-started) for the full guide.

---

## Without a server (local-only cache)

Do not pass `connect` to `createCache()`. The WASM module runs as a pure in-memory cache with TTL — no server, no WebSocket, no backend changes required.

```typescript
import { createCache } from 'recached-edge'

const cache = await createCache() // no connect option — local-only

async function getUser(id: number): Promise<User> {
  const key = `user:${id}`
  const cached = cache.getJSON<User>(key)
  if (cached !== null) return cached

  const user: User = await fetch(`/api/users/${id}`).then(r => r.json())
  cache.setJSON(key, user, 60) // cache for 60s
  return user
}

async function getProducts(): Promise<Product[]> {
  const cached = cache.getJSON<Product[]>('products')
  if (cached !== null) return cached

  const products: Product[] = await fetch('/api/products').then(r => r.json())
  cache.setJSON('products', products, 300) // cache for 5 minutes
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

## Manual reactivity (non-React frameworks)

`onMutation` fires whenever the local store changes — from a local write, a server push, or a cross-tab BroadcastChannel message. It is the low-level hook used by `useKey` and `useKeyJSON` internally.

```typescript
// Svelte
import { onMount, onDestroy } from 'svelte'
import { writable } from 'svelte/store'
import { cache } from '../lib/cache'

export let productId: string

const key = `stock:${productId}`
const stock = writable<string | null>(cache.get(key))

let unsubscribe: () => void

onMount(() => {
  stock.set(cache.get(key))
  unsubscribe = cache.onMutation(() => stock.set(cache.get(key)))
})

onDestroy(() => unsubscribe?.())
```

The callback receives no arguments — it signals that something changed. Read the specific key you care about inside the callback.

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

import { createCache } from 'recached-edge'
import { cache as cacheRef } from '../lib/cache'

export function CacheProvider({ children }: { children: React.ReactNode }) {
  // WASM must be initialized in a client component (after hydration)
  return <>{children}</>
}
```

Use `@recached/react` for the recommended Next.js App Router integration — it wraps WASM init and the cache lifecycle automatically.

### webpack

Add to your webpack config:

```javascript
module.exports = {
  experiments: {
    asyncWebAssembly: true,
  },
}
```
