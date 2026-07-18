# Security & Production Checklist

Recached ships **insecure by default**, like Redis: no password, no TLS, no IP restrictions. That is
convenient for `localhost` development and dangerous anywhere else. This page is the checklist to
work through before exposing it.

::: danger Never expose an unauthenticated Recached to the internet
With no `RECACHED_PASSWORD`, anyone who can reach port 6379 can read every key, write any key, and
run `FLUSHDB`. This is the single most common way self-hosted caches get compromised.
:::

## Minimum production checklist

- [ ] **Set `RECACHED_PASSWORD`** to a long random secret.
- [ ] **Bind to a private interface** — `RECACHED_BIND=127.0.0.1` or a VPC address, never `0.0.0.0`
      on a public host.
- [ ] **Enable TLS** (`RECACHED_TLS_CERT` + `RECACHED_TLS_KEY`) if traffic crosses any network you do
      not control.
- [ ] **Firewall the metrics port** — it has no authentication of its own.
- [ ] **Set `RECACHED_SYNC_SECRET`** before any browser connects to a multi-tenant deployment.
- [ ] **Set `RECACHED_MAX_MEMORY` and/or `RECACHED_MAX_KEYS`** so an unbounded keyspace cannot
      exhaust the host.
- [ ] **Set `RECACHED_REPL_PASSWORD`** if you run replication.
- [ ] Review [what is still beta](/guide/introduction#maturity) before putting the sync layer in
      front of untrusted users.

## Authentication

```bash
RECACHED_PASSWORD="$(openssl rand -base64 32)" recached-server
```

Clients must `AUTH` before any other command; unauthenticated commands are refused with
`NOAUTH Authentication required.`

Two properties worth knowing:

- **Password comparison is constant-time**, so it does not leak the secret through timing.
- **Five consecutive failures close the connection.** This slows brute force but does not ban the
  source — an attacker can reconnect. Pair it with an IP allowlist or a firewall if you are exposed
  to untrusted networks.

## Transport encryption

```bash
RECACHED_TLS_CERT=./cert.pem RECACHED_TLS_KEY=./key.pem recached-server
```

TLS applies to **both** ports when enabled: clients use `rediss://` for RESP and `wss://` for the
browser sync socket.

::: warning TLS is all-or-nothing, and fails open
If either variable is missing, the server silently falls back to **plain TCP and plain WebSocket**.
There is no error and no refusal to start. After enabling TLS, verify it took effect rather than
assuming — connect with `rediss://` and confirm a plaintext client is refused.
:::

## IP allowlisting

```bash
RECACHED_ALLOW_IPS="10.0.1.5,10.0.1.6" recached-server
```

::: warning Exact IP addresses only — CIDR is not supported
Each entry must parse as a single IP. Anything else — `10.0.0.0/8`, a hostname, a typo — is
**logged as a warning and dropped from the list**.

The failure mode this creates is worth internalising: if *every* entry is invalid, the allowlist
becomes empty, and an empty allowlist rejects **every** connection. The server starts normally and
appears completely unreachable, with only a `warn!` line explaining why. It fails closed, which is
the safe direction, but it does not fail loudly.
:::

Treat the allowlist as defence in depth, not a primary control. In cloud environments where addresses
rotate, prefer TLS plus authentication and enforce network boundaries with security groups.

## The sync port is a different threat model

Port 6379 is reached by your backend. Port 6380 is reached by **browsers** — that is, by code running
on machines you do not control, in the hands of users who may be adversarial.

Without `RECACHED_SYNC_SECRET`, any browser that can open the WebSocket can read and write the entire
keyspace. For a single-tenant internal dashboard that may be fine. For anything multi-tenant it is a
full data breach.

```bash
RECACHED_SYNC_SECRET="$(openssl rand -base64 32)" recached-server
```

With the secret set, connections present an HMAC-signed token minted by your backend, and **every
command — reads included — is checked against the granted patterns**. Keyspace-wide and
administrative commands (`KEYS`, `SCAN`, `DBSIZE`, `FLUSHDB`, `SAVE`, `BGSAVE`, `REPLICAOF`) are
refused outright on scoped connections. Out-of-scope key access is refused with
`NOSCOPE key '<key>' is outside this connection's sync scopes`.

Read [Sync Scopes](/server/sync-scopes) in full before you design your token scheme — the model is
prefix-based and the details matter.

### Minting tokens

Tokens are minted **server-side**, from a session your backend has already authenticated. The
secret must never reach the browser:

1. User authenticates with your application as normal.
2. Your backend derives the patterns that user may touch — for example `cart:42:*`, `user:42:*`.
3. Your backend signs a token with `RECACHED_SYNC_SECRET` and returns it.
4. The browser passes it to `createCache({ connect: { syncToken } })`.

If a token leaks, it grants its scopes until it expires — scope narrowly and keep lifetimes short.

## Replication

Set `RECACHED_REPL_PASSWORD` so an attacker cannot attach a rogue replica and stream your entire
keyspace. Replication traffic is covered by TLS when TLS is enabled.

Note the failover model: single-replica automatic promotion only. In a multi-replica topology,
designate one replica for auto-failover and keep the rest passive, or you risk split-brain — see
[when Recached is not the right fit](/guide/introduction#when-recached-is-not-the-right-fit).

## Resource limits

An unbounded cache is a denial-of-service vector against its own host.

| Setting | Why |
|---|---|
| `RECACHED_MAX_MEMORY` | Caps memory; pair with `RECACHED_EVICTION` to choose behaviour at the cap. |
| `RECACHED_MAX_KEYS` | Caps keyspace size independently of value sizes. |
| `RECACHED_MAX_CONNECTIONS` | Defaults to 1024. Connections beyond the limit are rejected outright. |

See [Operations → Capacity limits](/server/operations#capacity-limits) for the limits that are
compiled in and cannot be configured.

## Threat model, stated plainly

What Recached defends against today:

- Unauthenticated access (password, constant-time comparison)
- Network eavesdropping (TLS on both ports)
- Browser clients reading data they should not (sync scopes, signed tokens)
- Brute-force password guessing (connection dropped after 5 failures)
- Rogue replicas (replication password)

What it does **not** defend against, and you should not assume:

- **No per-command ACLs.** Unlike Redis 6+ ACLs, authentication is all-or-nothing on the RESP port —
  any authenticated client can run any command. Scoping exists only on the WebSocket sync path.
- **No audit log.** There is no record of who read or wrote what.
- **No encryption at rest.** Snapshots and AOF files are plaintext MessagePack; protect them with
  filesystem permissions and disk encryption.
- **No rate limiting on the RESP port.** `RLSET`/`RLCHECK` are commands you can use for *your*
  application's rate limiting; they do not throttle clients of the cache itself.
- **No third-party security review.** The sync layer in particular is young. See
  [Maturity](/guide/introduction#maturity).

## Reporting a vulnerability

Report security issues privately to [dennis@thinkgrid.dev](mailto:dennis@thinkgrid.dev) rather than
in a public issue.
