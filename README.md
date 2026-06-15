<div align="center">
  <img src="recached.jpg" alt="Recached" width="800" />
  <h1>Recached</h1>
  <p><b>A Rust cache server that runs on your backend <em>and</em> inside the browser.</b></p>

  <a href="https://recached.dev"><img src="https://img.shields.io/badge/Docs-recached.dev-blue.svg" alt="Docs"></a>
  <a href="https://www.npmjs.com/package/recached-edge"><img src="https://img.shields.io/npm/v/recached-edge?label=npm" alt="npm"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Language-Rust-orange.svg?logo=rust" alt="Rust"></a>
  <a href="https://webassembly.org"><img src="https://img.shields.io/badge/Ecosystem-WebAssembly-yellow.svg" alt="Wasm"></a>
  <a href="LICENSE.md"><img src="https://img.shields.io/badge/License-MIT-green.svg" alt="MIT"></a>
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
docker run -p 6379:6379 -p 6380:6380 ghcr.io/thinkgrid-labs/recached:latest

# Homebrew (macOS)
brew tap thinkgrid-labs/recached && brew install recached && recached-server

# Cargo
cargo install recached && recached-server
```

```bash
# Browser / Edge (npm)
npm install recached-edge
```

---

## How it works

```
┌─────────────────┐        RESP (port 6379)        ┌──────────────────┐
│   Your backend  │ ──────────────────────────────► │  Recached Server │
└─────────────────┘                                 │  (server-native) │
                                                    └────────┬─────────┘
                                                             │ WebSocket
                                                             │ sync (6380)
                                                    ┌────────▼─────────┐
                                                    │  Browser / Edge  │
                                                    │  (wasm-edge)     │
                                                    │  local reads: 0ms│
                                                    └──────────────────┘
```

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

---

## Contributing

Recached is maintained by Dennis. Bug reports, PRs, and feedback are all welcome.

1. Fork the repo and create a branch: `git checkout -b feat/my-feature`
2. Make your changes — server logic lives in `server-native/`, WASM bindings in `wasm-edge/`
3. Run `cargo test --workspace` before opening a PR
4. Open a pull request with a clear description

Open an issue before large features or architectural changes. Areas where contributions are especially welcome:

- **Benchmarks** — `redis-benchmark` against Redis 7 on multi-core hardware
- **Client examples** — React, Vue, or SvelteKit demos using `recached-edge`
- **Bug reports** — edge cases in the RESP parser, TTL eviction, pub/sub delivery, or WebSocket sync

See [recached.dev/roadmap](https://recached.dev/roadmap) for what's planned.

Reach out: [dennis@thinkgrid.dev](mailto:dennis@thinkgrid.dev)

## Support Recached

Recached is free and open-source, maintained by one person. If it saves you infrastructure cost or development time, [sponsoring on GitHub](https://github.com/sponsors/thinkgrid-labs) directly funds continued development: more Redis commands, RESP3, cluster support, and performance work.

## License

MIT — © 2026 ThinkGrid Labs
