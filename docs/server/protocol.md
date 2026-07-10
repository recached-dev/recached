# Wire Protocol

This page is **normative**: client SDKs (browser `recached-edge`, the planned mobile bindings) and the server implement exactly what is written here. If code and this page disagree, one of them has a bug. The reference client implementation is the platform-neutral [`sync-client`](https://github.com/thinkgrid-labs/recached/tree/main/sync-client) crate.

## Transports

| Port | Transport | Framing | Audience |
|---|---|---|---|
| 6379 | TCP | RESP2, pipelined | Trusted backends (any Redis client) |
| 6380 | WebSocket | One RESP value per **text** frame | Untrusted browsers / apps |

WebSocket text frames imply UTF-8: raw binary values are only fully round-trippable over TCP.

## Frame taxonomy (WebSocket)

Every frame a client receives is exactly one of:

| Kind | Shape | Meaning |
|---|---|---|
| **Reply** | any RESP value not matching the rows below | Response to one command this connection sent |
| **Mutation push** | RESP3 Push `>N` whose elements form a replayable command (`SET`, `HSET`, `JSET`, `JMERGE`, …) | Another client/backend mutated a key in scope — apply to the local store |
| **Pub/sub push** | RESP3 Push `>3` = `["message", channel, payload]` | Pub/sub delivery |
| **Keychange push** | Array `["keychange", key, value]` | A watched / live-queried key changed. `value`: full string, nil (deleted), or a type-name marker (`hash`, `list`, `set`, `zset`, `json`, `ratelimit`) whose content travels via mutation pushes instead |
| **Query state** | Array `["qstate", pattern, k1, v1, …]` | **Both** the reply to a `QSUB` **and** initial state to apply (same value encoding as keychange) |

## The ordering invariant (acknowledgment correlation)

> The server sends **exactly one reply per command, in the order commands were received**. Pushes may interleave anywhere, but are always distinguishable by the table above.

This is the invariant that makes reliable delivery cheap: a client keeps a FIFO of sent commands; each incoming *reply* acknowledges the oldest entry. `qstate` counts as the reply to its `QSUB`; `keychange` and all `>` pushes are never replies. An acknowledged write is durably done and can be retired from the client's outbox.

## Session establishment

On every (re)connect, in this order:

1. `AUTH password` — required first when `RECACHED_PASSWORD` is set
2. `SYNC TOKEN token` (strict scoping) or `SYNC pattern…` (open mode) — see [Sync Scopes](/server/sync-scopes)
3. `QSUB pattern` per live query — the `qstate` replies re-hydrate local state
4. Replay of the outbox (unacknowledged writes), oldest first

## Exactly-once writes (`DEDUP`)

Store writes that must not double-apply on replay are wrapped:

```
DEDUP <client-id> <wire-id> <command> <args…>
```

- `client-id`: 1–64 chars, stable per client, **unguessable** (a guessable id lets another authenticated client poison your high-water mark); browsers use `crypto.randomUUID()`
- `wire-id`: `(session-epoch << 32) | write-counter` — strictly increasing across a client's lifetime, including across page reloads (the epoch is persisted and bumped per session)
- Server behavior: per `client-id`, ids at or below the high-water mark reply `+DUP` and do **not** execute; higher ids execute and raise the mark (marked *before* execution). Marks are in server memory, swept after 24 h idle.
- `+DUP` is a normal reply — it acknowledges and retires the write.
- Scope checks, replica write-rejection, and metrics apply to the *wrapped* command.
- Connection-scoped commands (`SUBSCRIBE`, `UNSUBSCRIBE`, `PUBLISH`) are **never** wrapped — a replayed subscribe must re-execute, not be skipped.

## Token format (strict scoping)

```
base64url(payload) "." base64url(hmac_sha256(secret, base64url(payload)))
payload = "pattern1,pattern2[|unix-expiry-seconds]"
```

The HMAC is computed over the base64url payload *text*. Expiry is validated when the token is presented.

## Limits

| Limit | Value |
|---|---|
| `qstate` initial-state entries | 10 000 keys |
| Live queries per connection | 64 |
| Client outbox | 10 000 writes (oldest dropped) |
| `DEDUP` client id | 64 chars |
| Watched keys per connection | 1 024 |
| Bulk string / total message | 64 MB |

## Compatibility rules

- **Snapshot format**: enum variants are append-only (rmp-serde encodes by index) — old snapshot files must always load.
- **Tagged frames**: new server-initiated Array frames must carry a new first-element tag (like `keychange` / `qstate`); clients ignore unknown tags. Untagged Arrays are replies by definition.
- **New commands** never change the one-reply-per-command invariant.
- **Client IndexedDB schema**: bumps create missing object stores idempotently; existing stores are never dropped in an upgrade.
