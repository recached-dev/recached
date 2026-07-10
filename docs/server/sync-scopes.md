# Sync Scopes

By default, every mutation on the server is pushed to **every** connected WebSocket client. That is the right behavior for a single-user dashboard or an internal tool — and a data leak for a multi-user application, where user A's session keys would be pushed into user B's browser.

Sync scopes fix this. Each WebSocket connection declares which keys it may see, the server authorizes the declaration, and the mutation fan-out delivers only matching keys.

## The SYNC command

WebSocket-only (the TCP port is for trusted backends and is unaffected).

| Form | Behavior |
|---|---|
| `SYNC` | Returns this connection's current scope patterns. |
| `SYNC TOKEN <token>` | Sets scopes from a signed token — requires `RECACHED_SYNC_SECRET` on the server. |
| `SYNC <pattern> [pattern ...]` | Sets scopes directly. Only allowed when no secret is configured — this is a bandwidth filter, **not** an authorization boundary. |

Patterns are the same globs `KEYS` uses: `*`, `?`, `[abc]` — e.g. `cart:42:*`, `catalog:*`.

## Two modes

**Open mode** (no `RECACHED_SYNC_SECRET` set — the default) is backward compatible: connections that never call `SYNC` receive every mutation, exactly as before. A connection that calls `SYNC cart:*` receives only matching keys — useful for cutting bandwidth, but any client can choose any patterns, so it protects nothing.

**Strict mode** (`RECACHED_SYNC_SECRET` set) makes scopes an authorization boundary:

- A connection receives **no pushes** and may run **no key commands** until it presents a valid `SYNC TOKEN`.
- Every command is then checked against the granted scopes — reads and writes alike. `GET secret-key` outside your scopes returns `-NOSCOPE`, the same as a write.
- Keyspace-wide and administrative commands (`KEYS`, `SCAN`, `DBSIZE`, `FLUSHDB`, `SAVE`, `BGSAVE`, `REPLICAOF`) are refused entirely on scoped connections.
- Literal `SYNC <pattern>` is rejected — patterns must come from a signed token.

`PING`, `AUTH`, `MULTI`/`EXEC`/`DISCARD`, and pub/sub commands are always available. Note that pub/sub **channels** are not keys and are not scoped — don't put per-user secrets on broadcast channels.

## Scope tokens

A token is minted by your application backend — the only party that knows which user is asking — and handed to the browser client:

```
base64url(payload) + "." + base64url(hmac_sha256(secret, base64url(payload)))
```

The payload is comma-separated patterns, with an optional `|<unix-expiry-seconds>` suffix. Minting in Node.js:

```js
import crypto from 'node:crypto';

function mintSyncToken(secret, patterns, expiresInSecs = 3600) {
  const expiry = Math.floor(Date.now() / 1000) + expiresInSecs;
  const payload = Buffer.from(`${patterns.join(',')}|${expiry}`)
    .toString('base64url');
  const sig = crypto.createHmac('sha256', secret)
    .update(payload)
    .digest('base64url');
  return `${payload}.${sig}`;
}

// In your session/login handler:
const token = mintSyncToken(process.env.RECACHED_SYNC_SECRET, [
  `cart:${userId}:*`,
  `profile:${userId}`,
  'catalog:*',
]);
// → include the token in the page payload / session API response
```

The HMAC is computed over the base64url payload *text*, so there are no byte-canonicalization pitfalls — one `createHmac` call as shown is the whole minting story in any language.

The browser client then sends, over its WebSocket connection:

```
SYNC TOKEN <token>
```

and receives the granted patterns as confirmation. From that moment it receives pushes for — and can operate on — exactly those keys.

::: warning Expiry is checked at presentation time
The token's expiry is validated when `SYNC TOKEN` is presented, not continuously. An established connection keeps its scopes for its lifetime. Short-lived tokens bound the window in which a leaked token is usable — they do not cut off live connections.
:::

## Server setup

```bash
RECACHED_SYNC_SECRET="a-long-random-secret" \
RECACHED_PASSWORD="another-secret" \
RECACHED_BIND="0.0.0.0" \
recached-server
```

Use both secrets: `RECACHED_PASSWORD` gates who may connect at all; `RECACHED_SYNC_SECRET` gates what each connection may see. For browser deployments the WS port is public by definition — strict mode is the difference between "any visitor sees the whole keyspace" and "each visitor sees their own keys".

## Interaction with transactions and WATCH

Scope checks run before a command is queued inside `MULTI`, so an out-of-scope command errors at queue time rather than sneaking into `EXEC`. `WATCH` is scope-checked like any other key command — a connection cannot observe keys outside its grant.

## Replication and multi-tier setups

Scope filtering happens per-connection at the fan-out edge. Replicas receive the full write stream (they serve their own WebSocket clients, which are scoped independently), and the AOF/snapshot are unaffected.
