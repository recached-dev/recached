# Configuration

Recached is configured entirely through environment variables. There is no config file.

## Environment variable reference

| Variable | Default | Description |
|---|---|---|
| `RECACHED_PASSWORD` | _(none)_ | Require clients to authenticate with `AUTH <password>`. If unset, the server accepts connections without authentication. After 5 consecutive failed `AUTH` attempts, the connection is closed. |
| `RECACHED_ALLOW_IPS` | _(allow all)_ | Comma-separated list of IP addresses allowed to connect. Any connection from an IP not in the list is immediately closed. Invalid entries are logged and skipped. |
| `RECACHED_MAX_KEYS` | _(unlimited)_ | Maximum number of keys in the store. When this limit is reached, behavior depends on `RECACHED_EVICTION`. If set to `noeviction` (the default), write commands that would exceed the cap return an error. |
| `RECACHED_EVICTION` | `noeviction` | Eviction policy when `RECACHED_MAX_KEYS` is reached. See eviction policies below. |
| `RECACHED_METRICS_PORT` | `9091` | Port for the Prometheus metrics HTTP server. Metrics are available at `/metrics`. Set to `0` to disable. |
| `RECACHED_TLS_CERT` | _(none)_ | Path to a PEM-encoded TLS certificate file. TLS is enabled on both ports when this and `RECACHED_TLS_KEY` are set. |
| `RECACHED_TLS_KEY` | _(none)_ | Path to a PEM-encoded TLS private key file. If either `RECACHED_TLS_CERT` or `RECACHED_TLS_KEY` is missing, the server falls back to plain TCP/WS. |
| `RUST_LOG` | `info` | Log level. Accepts `error`, `warn`, `info`, `debug`, `trace`. Module-specific: `RUST_LOG=recached=debug,tokio=warn`. |

---

## Eviction policies

Eviction runs when `RECACHED_MAX_KEYS` is reached and a write command would add a new key.

| Policy | Behavior |
|---|---|
| `noeviction` | Write commands return an error when the key cap is reached. Existing keys are never evicted. Default behavior. |
| `lru` | Evicts the least-recently-used key. Applies to all keys regardless of TTL. |
| `allkeys-random` | Evicts a randomly selected key. Lower overhead than LRU. |
| `volatile-lru` | Evicts the least-recently-used key that has a TTL set. If no keys have a TTL, falls back to `noeviction`. |
| `volatile-ttl` | Evicts the key with the shortest remaining TTL. Prioritizes keys that are closest to expiring. Falls back to `noeviction` if no keys have a TTL. |

For most applications, `lru` is the right default when a key cap is configured.

---

## Common configurations

### Development (no auth, verbose logging)

```bash
RUST_LOG=debug recached-server
```

### Production with auth and key cap

```bash
RECACHED_PASSWORD="a-strong-random-secret" \
RECACHED_MAX_KEYS="1000000" \
RECACHED_EVICTION="lru" \
RECACHED_METRICS_PORT="9091" \
RUST_LOG="info" \
recached-server
```

### With IP allowlist (restrict to local network)

```bash
RECACHED_PASSWORD="secret" \
RECACHED_ALLOW_IPS="127.0.0.1,10.0.0.0/8,192.168.1.100" \
recached-server
```

Note: `RECACHED_ALLOW_IPS` accepts individual IP addresses. CIDR notation support depends on the version — check the release notes. If only specific hosts should connect, prefer TLS + auth over IP allowlisting in cloud environments where IPs rotate.

### With TLS (secure RESP and WSS)

Generate a self-signed certificate for local testing:

```bash
openssl req -x509 -newkey rsa:4096 -keyout key.pem -out cert.pem \
  -days 365 -nodes -subj "/CN=localhost"
```

Start with TLS:

```bash
RECACHED_TLS_CERT="./cert.pem" \
RECACHED_TLS_KEY="./key.pem" \
RECACHED_PASSWORD="secret" \
recached-server
```

Clients connecting over RESP must now use `rediss://` (RESP over TLS). Browser clients use `wss://` instead of `ws://`.

```typescript
// Backend
const cache = new Redis('rediss://127.0.0.1:6379')

// Browser
cache.connect('wss://your.domain:6380')
```

For a production certificate, use Let's Encrypt via `certbot` or provide the cert/key from your certificate authority.

### Prometheus metrics scrape

```bash
RECACHED_METRICS_PORT="9091" recached-server
```

Configure your Prometheus instance to scrape:

```yaml
scrape_configs:
  - job_name: recached
    static_configs:
      - targets: ['localhost:9091']
    metrics_path: /metrics
    scrape_interval: 15s
```

Available metrics include key count, command latency histograms, active connections, and WebSocket client count.

### High-connection workloads

The server accepts up to 1024 concurrent connections by default (enforced with a connection semaphore). This limit is not currently configurable via environment variable — if you need more, build from source and adjust the constant in `server-native/src/main.rs`.

For most web applications, 1024 concurrent connections to the cache server is more than enough. Browser clients each hold one WebSocket connection; backend services typically hold a small connection pool (2–10 connections).

---

## Notes on sensitive configuration

- Never commit `RECACHED_PASSWORD` to source control. Use an environment file (see [Installation — systemd service](/server/installation#systemd-service)) or a secrets manager (Vault, AWS Secrets Manager, Doppler).
- The password is compared in constant time to prevent timing attacks, but the brute-force lockout (5 failed attempts → disconnect) is the primary protection. Use a long random password.
- TLS is strongly recommended for any deployment where the cache server is reachable over a network that you do not fully control. Without TLS, `RECACHED_PASSWORD` is sent in plaintext on initial `AUTH`.
