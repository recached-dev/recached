---
layout: home
title: "Recached — Rust Cache for Backend and Browser"
description: "A Rust cache server that runs natively on your backend and as WebAssembly in the browser. Redis-compatible on the server, zero-latency local reads in the browser."

hero:
  name: "Recached ⚡"
  text: "Cache that runs everywhere."
  tagline: "Redis-compatible on your server. WebAssembly in the browser. Zero-latency reads. Automatic sync."
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
      link: https://github.com/thinkgrid-labs/recached
    - theme: alt
      text: npm
      link: https://www.npmjs.com/package/recached-edge

features:
  - icon: ⚡
    title: Zero-latency local reads
    details: The browser WASM module holds a live copy of the cache in local memory. Reads never leave the browser — no network hop, no round-trip.
  - icon: 🔄
    title: Automatic WebSocket sync
    details: Any mutation on the server is pushed to all connected browser instances instantly. Any write from the browser is pushed to the server and fanned out to other tabs.
  - icon: 🦀
    title: Redis-compatible server
    details: Speaks RESP on port 6379. Drop it in front of any Redis client — ioredis, node-redis, redis-py — with no code changes. Values are text; base64-encode binary payloads.
  - icon: 🌐
    title: Offline-first browser cache
    details: IndexedDB persistence means the cache survives page refreshes. Users see their data immediately, before any network request completes.
  - icon: 📡
    title: Cross-tab sync
    details: BroadcastChannel support means all tabs in the same browser share mutations automatically, with no server connection required.
  - icon: 🔒
    title: Production-ready server
    details: TLS, Prometheus metrics, password auth, IP allowlists, connection limits, eviction policies, and observable keys out of the box.
---

## What is Recached?

Every caching solution forces a choice: server-side caches like Redis mean every frontend read is a network round-trip; client-side state like Zustand or SWR means two caches — one on the server and one in every client, with manual staleness code gluing them together. **Recached removes the choice.**

The same Rust cache engine runs natively on your server (RESP on port 6379 — any Redis client works today, zero code changes; values are text, so binary payloads need base64) and as WebAssembly inside the browser. Reads always come from local WASM memory. The WebSocket is only a sync path, not a read path.

```typescript
import { createCache } from 'recached-edge'

const cache = await createCache({
  persistence: true,                          // survives page refresh via IndexedDB
  connect: { url: 'ws://localhost:6380' },    // syncs with the server
})

cache.get('inventory:item:99') // "42" — from local WASM memory, 0 ms

// React to any store mutation — local writes, server push, or cross-tab sync
cache.onMutation(() => {
  document.body.dataset.theme = cache.get('user:theme') ?? 'light'
})
```

No polling. No extra state management library. No round-trips for reads. The server is your backend's cache; the WASM module is your frontend's cache; the WebSocket is the invisible sync layer between them.
