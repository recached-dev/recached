---
layout: home
title: "Recached — Rust Cache for Backend and Browser"
description: "A Rust cache server that runs natively on your backend and as WebAssembly in the browser. A Redis-compatible command subset on the server and local reads in the browser."

hero:
  name: "Recached ⚡"
  text: "Cache that runs everywhere."
  tagline: "A Redis-compatible server. WebAssembly in the browser. Local reads without a network hop."
  image:
    src: /recached.jpg
    alt: Recached
  actions:
    - theme: brand
      text: Get Started
      link: /guide/quick-start
    - theme: alt
      text: How It Works
      link: /guide/how-it-works
    - theme: alt
      text: GitHub
      link: https://github.com/recached-dev/recached
    - theme: alt
      text: npm
      link: https://www.npmjs.com/package/recached-edge

features:
  - icon: ⚡
    title: Local reads without a network hop
    details: The browser WASM module holds a live copy of the cache in local memory. Reads never leave the browser — no network hop, no round-trip.
  - icon: 🔄
    title: Automatic WebSocket sync
    details: Any mutation on the server is pushed to all connected browser instances instantly. Any write from the browser is pushed to the server and fanned out to other tabs.
  - icon: 🦀
    title: Redis-compatible command subset
    details: Speaks RESP on port 6379 and works with common clients such as ioredis, node-redis, and redis-py. Check COMMAND for the supported subset before migrating.
  - icon: 🌐
    title: Offline-first browser cache
    details: IndexedDB persistence means the cache survives page refreshes. Users see their data immediately, before any network request completes.
  - icon: 📡
    title: Cross-tab sync
    details: BroadcastChannel support means all tabs in the same browser share mutations automatically, with no server connection required.
  - icon: 🔒
    title: Hardened cache server
    details: TLS, Prometheus metrics, password authentication, IP allowlists, connection limits, bounded eviction, and ordered replication. Release-candidate maturity.
---

## What is Recached?

Every caching solution forces a choice: server-side caches like Redis mean every frontend read is a network round-trip; client-side state like Zustand or SWR means two caches — one on the server and one in every client, with manual staleness code gluing them together. **Recached removes the choice.**

The same Rust cache engine runs natively on your server (RESP on port 6379) and as WebAssembly inside the browser. Common Redis clients work with the commands Recached implements. Browser reads come from local WASM memory; the WebSocket is a sync path, not a read path.

```typescript
import { createCache } from 'recached-edge'

const cache = await createCache({
  persistence: true,                          // survives page refresh via IndexedDB
  connect: { url: 'ws://localhost:6380' },    // syncs with the server
})

cache.get('inventory:item:99') // "42" — local WASM memory, no network request

// React to any store mutation — local writes, server push, or cross-tab sync
cache.onMutation(() => {
  document.body.dataset.theme = cache.get('user:theme') ?? 'light'
})
```

No polling. No extra state management library. No round-trips for reads. The server is your backend's cache; the WASM module is your frontend's cache; the WebSocket is the invisible sync layer between them.

### Or just the browser half

The sync layer is optional. Omit `connect` and no socket is opened: `recached-edge` becomes a
standalone client cache — TTLs, counters, JSON documents, glob queries, IndexedDB persistence and
cross-tab sync — with no Recached server anywhere and no changes to your backend.

```typescript
const cache = await createCache({ persistence: true, broadcastChannel: 'my-app' })
cache.setJSON('user:42', user, 60) // expires on its own, survives a refresh
```

Pub/sub, live queries and cross-device sync are the parts that need a server. See
[no server at all](/guide/use-cases#no-server-at-all).
