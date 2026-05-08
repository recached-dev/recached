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

features:
  - icon: ⚡
    title: Zero-latency local reads
    details: The browser WASM module holds a live copy of the cache in local memory. Reads never leave the browser — no network hop, no round-trip.
  - icon: 🔄
    title: Automatic WebSocket sync
    details: Any mutation on the server is pushed to all connected browser instances instantly. Any write from the browser is pushed to the server and fanned out to other tabs.
  - icon: 🦀
    title: Redis-compatible server
    details: Speaks RESP on port 6379. Drop it in front of any Redis client — ioredis, node-redis, redis-py — with no code changes.
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

Recached is an in-memory cache written in Rust that solves a problem no other cache tool addresses: the same engine runs natively on your server **and** as WebAssembly inside the browser, with both sides kept in sync over WebSockets.

On the backend, it speaks RESP on port 6379 — any Redis client works against it today without code changes. In the browser, you import it as a `.wasm` module and get zero-latency local reads with automatic background sync to the server.

```typescript
import init, { RecachedCache } from 'recached-edge'

await init()
const cache = new RecachedCache()

// Connect to the server — mutations sync both ways automatically
cache.connect('ws://localhost:6380')

cache.set('user:theme', 'dark')
console.log(cache.get('user:theme')) // 'dark' — read from local WASM memory, 0 ms

// When your backend does SET user:theme light over RESP,
// this browser instance receives the update automatically.
cache.watch('user:theme', (newValue) => {
  document.body.dataset.theme = newValue ?? 'light'
})
```

No polling. No extra state management library. No round-trips for reads. The server is your backend's cache; the WASM module is your frontend's cache; the WebSocket is the invisible sync layer between them.
