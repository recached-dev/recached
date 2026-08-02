<div align="center">
  <img src="recached.jpg" alt="Recached" width="800" />
  <h1>Recached</h1>
  <p><b>A Rust cache server that runs on your backend <em>and</em> inside the browser.</b></p>

  <a href="https://recached.dev"><img src="https://img.shields.io/badge/Docs-recached.dev-blue.svg" alt="Docs"></a>
  <a href="https://www.npmjs.com/package/recached-edge"><img src="https://img.shields.io/npm/v/recached-edge?label=npm" alt="npm"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Language-Rust-orange.svg?logo=rust" alt="Rust"></a>
  <a href="https://webassembly.org"><img src="https://img.shields.io/badge/Ecosystem-WebAssembly-yellow.svg" alt="Wasm"></a>
  <a href="LICENSE.md"><img src="https://img.shields.io/badge/License-Apache_2.0-green.svg" alt="Apache 2.0"></a>
</div>

---

Every caching solution forces a choice: server-side caches like Redis mean every frontend read is a network round-trip; client-side state like Zustand or SWR means two caches — one on the server and one in every client, with manual staleness code gluing them together. **Recached removes the choice.**

The same Rust cache engine runs natively on your server (RESP on port 6379 — any Redis client works today, zero code changes) and as WebAssembly inside the browser. Reads always come from local WASM memory. The WebSocket is only a sync path, not a read path.

> [!NOTE]
> Recached is not a full Redis replacement. It covers the subset most applications actually need: strings, expiry, counters, all collection types, transactions, pub/sub, and observable keys. Best fit: reactive UIs, session caches, browser-side API response caching, and rate limiting.

**→ Full documentation, use cases, API reference, and guides at [recached.dev](https://recached.dev)**

---

## Install

```bash
# Docker
docker run -p 6379:6379 -p 6380:6380 ghcr.io/recached-dev/recached:latest

# Homebrew (macOS)
brew tap recached-dev/recached && brew install recached && recached-server

# Cargo
cargo install recached && recached-server
```

```bash
# Browser / Edge (npm)
npm install recached-edge
```

> [!IMPORTANT]
> **`recached-edge` 0.1.1 – 0.2.0 do not work in the browser.** `core-engine` read the clock via
> `std::time::SystemTime::now()`, which panics on `wasm32-unknown-unknown`. The clock is read on
> nearly every operation, so **no store write completes** on those versions — and a client whose
> write-ahead log passed the compaction threshold erased its own persisted cache before the
> replacement snapshot was written.
>
> Fixed in **0.2.1** — install `recached-edge@^0.2.1` or later.
>
> **The server is unaffected.** It runs on a native target where the clock works normally; this is
> browser/WASM only.

---

## How it works

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/architecture-dark.svg">
    <img src="assets/architecture-light.svg" width="880" alt="Your backend writes to the Recached server over RESP on port 6379. The server syncs over a WebSocket on port 6380 to the browser or edge runtime, where reads are served from local WebAssembly memory. Writes flow back the same way.">
  </picture>
</p>

Any mutation on the server is pushed to all connected browser instances automatically. Any write from the browser is pushed to the server and fanned out to other clients. Reads always come from local WASM memory — no network hop.

---

## Quick look

**Backend** — any Redis client, port 6379:

```javascript
import Redis from 'ioredis';
const cache = new Redis('redis://127.0.0.1:6379');
await cache.set('inventory:item:99', '42');
```

**Browser** — WebAssembly, port 6380:

```typescript
import { createCache } from 'recached-edge';

const cache = await createCache({
  persistence: true,                        // survives page refresh via IndexedDB
  connect: { url: 'ws://127.0.0.1:6380' }, // syncs with the server
});

cache.get('inventory:item:99'); // "42" — from local WASM memory, 0 ms
```

Both examples are plaintext, which is the default. Set `RECACHED_TLS_CERT` and `RECACHED_TLS_KEY`
and the same ports serve TLS — connect with `rediss://` and `wss://` instead. Before exposing either
port beyond localhost, work through
[recached.dev/server/security](https://recached.dev/server/security): a default server has no
password, no TLS, and no restriction on which web pages may open the sync socket.

---

## Benchmarks

Measured with `redis-benchmark` (100k requests, 50 connections, 64-byte values, randomized keys, persistence disabled on all servers) on a 4-core Intel i5-8259U laptop, July 2026 — Recached v0.1.8 vs Redis 7.2.5 vs Valkey 9.1.0, one server at a time. Current release is v0.2.4; these command paths were A/B tested across the v0.2.4 changes and moved within run-to-run noise, but the three-way suite has not been re-run since v0.1.8.

Pipelined (`-P 16`) — raw command throughput, requests/sec, **bold** = best per row:

| Command | Recached | Redis 7.2.5 | Valkey 9.1.0 |
|---|---:|---:|---:|
| SET | **421,941** | 375,940 | 294,118 |
| GET | **546,448** | 512,821 | 483,092 |
| INCR | **448,430** | 421,941 | 413,223 |
| LPUSH | **473,934** | 409,836 | 386,100 |
| SADD | 421,941 | **462,963** | 378,788 |
| HSET | **408,163** | 324,675 | 287,356 |
| ZADD | **414,938** | 197,628 | 221,239 |

Recached's multi-threaded runtime spreads connections across all cores, while Redis and Valkey execute commands on one — pipelined, Recached comes out ahead of Redis on 6 of 7 commands and ahead of Valkey on all 7, on the same hardware. Unpipelined (one command per round-trip — the traffic shape of typical request-scoped cache calls), the localhost round-trip dominates and Recached runs at 46–96% of Redis with sub-millisecond p50 latency on every common command (GET 58.1k vs 61.6k rps; HSET is the weakest at 46%).

**New in v0.2.4:** sorted sets gained a score-ordered index, so range reads no longer sort the whole set on every query. On a ~45k-member leaderboard, repeated `ZRANGE key 0 9` went from 244 to 133k rps (**546×**), and an alternating `ZADD` + `ZRANGE` loop from 65 s to 0.11 s (**597×**). `ZADD` gives up ~15% against a set that is actively being read; a write-only sorted set never builds the index and is unaffected. Measured as before/after ratios on a loaded machine — see the [benchmarks page](https://recached.dev/guide/benchmarks) for methodology.

Full tables with latency percentiles, pipelined results, methodology, and known hotspots: **[recached.dev/guide/benchmarks](https://recached.dev/guide/benchmarks)**. Reproduce with [`scripts/benchmark.sh`](scripts/benchmark.sh) — results from server-grade hardware welcome.

---

## Maturity

Being honest about where things stand:

- **The cache server is production-ready for cache workloads** — persistence, replication with auto-failover, TLS, hardened parsers, metrics, and a load/chaos CI suite. Treat it as a cache, not a system of record.
- **The sync layer (browser sync, live queries, offline outbox, scoped auth) is beta** — the invariants are [specified](https://recached.dev/server/protocol) and tested end-to-end, but the code is young and hasn't had real-world miles or third-party security review yet. Don't put the WebSocket port on the public internet for multi-tenant data without reading [Sync Scopes](https://recached.dev/server/sync-scopes) first.

The road to 1.0 is hardening, not features. Bug reports from production-like use are the most valuable contribution right now.

---

## Contributing

Bug reports, PRs, and feedback are all welcome.

1. Fork the repo and create a branch: `git checkout -b feat/my-feature`
2. Make your changes — server logic lives in `server-native/`, WASM bindings in `wasm-edge/`
3. Run `cargo test --workspace` before opening a PR
4. Open a pull request with a clear description

Open an issue before large features or architectural changes. Areas where contributions are especially welcome:

- **Benchmarks** — run [`scripts/benchmark.sh`](scripts/benchmark.sh) on multi-core server hardware and share the results
- **Client examples** — React, Vue, or SvelteKit demos using `recached-edge`
- **Bug reports** — edge cases in the RESP parser, TTL eviction, pub/sub delivery, or WebSocket sync

See [recached.dev/roadmap](https://recached.dev/roadmap) for what's planned.

Reach out: [dennis@thinkgrid.dev](mailto:dennis@thinkgrid.dev)


## Support Recached

Recached is free and open-source, maintained by one person. If it saves you infrastructure cost or development time, [sponsoring on GitHub](https://github.com/sponsors/thinkgrid-labs) directly funds continued development: more Redis commands, RESP3, cluster support, and performance work.

## License

Apache License 2.0 — © 2026 ThinkGrid Labs
