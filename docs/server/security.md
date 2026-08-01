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
      not control. Note this covers the RESP and WebSocket ports only — see
      [Replication](#replication).
- [ ] **Firewall the metrics port** — it has no authentication of its own, and
      `RECACHED_ALLOW_IPS` does not apply to it.
- [ ] **Set `RECACHED_SYNC_SECRET`** before any browser connects to a multi-tenant deployment.
- [ ] **Set `RECACHED_ALLOWED_ORIGINS`** to the origins your application is served from, so that
      other pages in a user's browser cannot open the sync socket.
- [ ] **Set `RECACHED_MAX_MEMORY` and/or `RECACHED_MAX_KEYS`** so an unbounded keyspace cannot
      exhaust the host.
- [ ] **Set `RECACHED_REPL_PASSWORD`** if you enable the replication listener. The server refuses to
      start without it on any interface other than loopback.
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

TLS applies to **both client ports** when enabled: clients use `rediss://` for RESP and `wss://` for
the browser sync socket. It does not apply to the replication port or the metrics port, which are
always plaintext.

::: tip TLS is all-or-nothing, and fails loudly
Set both variables or neither. If exactly one is present the server **refuses to start** with an
explicit error, rather than falling back to plaintext — an operator who set `RECACHED_TLS_CERT`
intends TLS, and silently serving unencrypted traffic because the key variable was misspelled is a
failure you would not notice until traffic had already been exposed.

Prior to v0.2.1 this fell back to plain TCP and plain WebSocket silently. If you are on an earlier
version, verify TLS took effect rather than assuming: connect with `rediss://` and confirm a
plaintext client is refused.
:::

## IP allowlisting

```bash
RECACHED_ALLOW_IPS="10.0.1.5,10.0.1.6" recached-server
```

::: warning Exact IP addresses only — CIDR is not supported
Each entry must parse as a single IP address. Anything else — `10.0.0.0/8`, a hostname, a typo —
causes the server to **refuse to start**, naming the offending entry.

Prior to v0.2.1 invalid entries were logged and dropped, which quietly produced a *narrower*
allowlist than configured: a mistyped CIDR range excluded every host it was meant to admit, and a
wholly invalid list produced an empty allowlist that rejected every connection while the process
still started and passed health checks. Failing to start makes the misconfiguration unmissable.
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

### Restrict which pages may connect

Browsers apply neither CORS nor a preflight to WebSockets. That makes this port different from every
HTTP endpoint you have secured before: **any page a user visits can open a socket to any Recached
that page's browser can reach**, and the request carries the user's network position. On a developer
machine running `ws://localhost:6380`, that is every site in every tab.

```bash
RECACHED_ALLOWED_ORIGINS="https://app.example.com,https://admin.example.com" recached-server
```

A handshake from any other origin is refused with `403` before a single frame is exchanged. Three
things to understand about the limits of this control:

- **It only defends against browsers.** A native client omits `Origin`, and an attacker with a raw
  socket can send whatever they like — so a client that sends no `Origin` is admitted. What the
  allowlist separates is *the application you deployed* from *another page in the same browser*,
  which is the threat that is otherwise unaddressed.
- **It is not a substitute for `RECACHED_SYNC_SECRET`.** Origin says which page connected; a scope
  token says which keys that page may touch. A multi-tenant deployment needs both.
- **Unset means allow-all**, with a warning at startup. Set it before exposing 6380 to a browser.

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

::: danger The replication port bypassed authentication entirely before 0.2.4
Every release up to and including 0.2.3 bound `${RECACHED_BIND}:6381` — default `0.0.0.0` —
**unconditionally on every node**, whether or not replication was configured, and skipped the
handshake completely when `RECACHED_REPL_PASSWORD` was unset, which was also the default. Any peer
that connected received a full dump of the keyspace followed by a live stream of every subsequent
write, regardless of `RECACHED_PASSWORD`. If you are on an earlier version, upgrade or firewall
6381 now.
:::

The listener is opt-in from 0.2.4 onward:

```bash
RECACHED_REPL_ENABLE=1 \
RECACHED_REPL_PASSWORD="$(openssl rand -base64 32)" \
recached-server
```

- **It binds only when `RECACHED_REPL_ENABLE` is set** — on the primary, and on any replica that
  serves sub-replicas. A replica that merely consumes replication does not need it.
- **Enabling it off-loopback without a password refuses to start.** The port serves the whole
  keyspace to whoever connects, so it is not a thing to leave unauthenticated on a reachable
  interface.
- **`RECACHED_ALLOW_IPS` and `RECACHED_MAX_CONNECTIONS` apply to it**, which they did not before.
- **Failed auth is throttled per source address.** The handshake is one-shot, so before this a wrong
  guess cost an attacker only a reconnect.

::: warning Replication traffic is never encrypted
TLS covers the RESP and WebSocket ports. It does **not** cover port 6381 — both the listener and the
replica's outbound connection are plain TCP, so the password and the entire keyspace cross the
network in the clear. Earlier versions of this page claimed otherwise; that was wrong. Run
replication over a private network or a tunnel (WireGuard, an SSH tunnel, a service mesh), and treat
the replication password as protecting against a rogue replica rather than against an observer.

The replica also does not verify the primary's identity, so a DNS hijack or an on-path attacker can
feed it an arbitrary keyspace. The same mitigation applies.
:::

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
- Network eavesdropping on the RESP and WebSocket ports (TLS)
- Browser clients reading data they should not (sync scopes, signed tokens)
- Cross-origin WebSocket hijacking (`RECACHED_ALLOWED_ORIGINS`)
- Brute-force password guessing (connection dropped after 5 failures; replication auth throttled
  per source address)
- Rogue replicas (replication password, and an opt-in listener that will not run unauthenticated
  off-loopback)
- Connection-slot exhaustion by half-open sockets (handshake deadline)
- Local users reading the cache off disk (snapshot, AOF and dedup files created `0600`)

What it does **not** defend against, and you should not assume:

- **No per-command ACLs.** Unlike Redis 6+ ACLs, authentication is all-or-nothing on the RESP port —
  any authenticated client can run any command. Scoping exists only on the WebSocket sync path.
- **No audit log.** There is no record of who read or wrote what.
- **No encryption in transit for replication.** Port 6381 is always plaintext. See
  [Replication](#replication).
- **No encryption at rest.** Snapshots and AOF files are plaintext MessagePack. They are created
  `0600` so other local users cannot read them, but anyone who can read them as the server's user,
  or who obtains the disk, has the whole keyspace. Use disk encryption.
- **No rate limiting on the RESP port.** `RLSET`/`RLCHECK` are commands you can use for *your*
  application's rate limiting; they do not throttle clients of the cache itself.
- **No third-party security review.** The sync layer in particular is young. See
  [Maturity](/guide/introduction#maturity).

## Reporting a vulnerability

Report security issues privately to [dennis@thinkgrid.dev](mailto:dennis@thinkgrid.dev) rather than
in a public issue.
