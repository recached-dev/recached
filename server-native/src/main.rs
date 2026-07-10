// jemalloc isn't available under MSVC (see Cargo.toml); fall back to the
// system allocator there.
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use core_engine::cmd::{Command, SetExpiry, ZAddCondition};
use core_engine::resp::Value;
use core_engine::store::{EvictionPolicy, KeyValueStore, SnapshotEntry};
use futures_util::{SinkExt, StreamExt};
use metrics::{counter, gauge};
use rustls_pemfile::{certs, private_key};
use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, broadcast, mpsc};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

// ── metrics ───────────────────────────────────────────────────────────────────

/// RAII guard that tracks an active connection. Increments on creation,
/// decrements when dropped (i.e. when the handler future completes).
struct ConnectionGuard;

impl ConnectionGuard {
    fn tcp() -> Self {
        counter!("recached_connections_total", "type" => "tcp").increment(1);
        gauge!("recached_connections_active").increment(1.0);
        Self
    }

    fn ws() -> Self {
        counter!("recached_connections_total", "type" => "ws").increment(1);
        gauge!("recached_connections_active").increment(1.0);
        Self
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        gauge!("recached_connections_active").decrement(1.0);
    }
}

fn command_name(cmd: &Command) -> &'static str {
    match cmd {
        Command::Ping(_) => "ping",
        Command::Auth(_) => "auth",
        Command::Get(_) => "get",
        Command::Set(_, _, _) => "set",
        Command::Del(_) => "del",
        Command::Unlink(_) => "unlink",
        Command::Append(_, _) => "append",
        Command::Strlen(_) => "strlen",
        Command::GetSet(_, _) => "getset",
        Command::MGet(_) => "mget",
        Command::MSet(_) => "mset",
        Command::SetNx(_, _) => "setnx",
        Command::SetEx(_, _, _) => "setex",
        Command::PSetEx(_, _, _) => "psetex",
        Command::Incr(_) => "incr",
        Command::Decr(_) => "decr",
        Command::IncrBy(_, _) => "incrby",
        Command::DecrBy(_, _) => "decrby",
        Command::Expire(_, _) => "expire",
        Command::PExpire(_, _) => "pexpire",
        Command::ExpireAt(_, _) => "expireat",
        Command::PExpireAt(_, _) => "pexpireat",
        Command::Ttl(_) => "ttl",
        Command::PTtl(_) => "pttl",
        Command::Persist(_) => "persist",
        Command::Exists(_) => "exists",
        Command::Keys(_) => "keys",
        Command::Scan(_, _, _) => "scan",
        Command::DbSize => "dbsize",
        Command::FlushDb => "flushdb",
        Command::Rename(_, _) => "rename",
        Command::Type(_) => "type",
        Command::HSet(_, _) => "hset",
        Command::HGet(_, _) => "hget",
        Command::HGetAll(_) => "hgetall",
        Command::HDel(_, _) => "hdel",
        Command::HKeys(_) => "hkeys",
        Command::HVals(_) => "hvals",
        Command::HLen(_) => "hlen",
        Command::HIncrBy(_, _, _) => "hincrby",
        Command::HIncrByFloat(_, _, _) => "hincrbyfloat",
        Command::HExists(_, _) => "hexists",
        Command::HSetNx(_, _, _) => "hsetnx",
        Command::HMGet(_, _) => "hmget",
        Command::LPush(_, _) => "lpush",
        Command::RPush(_, _) => "rpush",
        Command::LPushX(_, _) => "lpushx",
        Command::RPushX(_, _) => "rpushx",
        Command::LPop(_, _) => "lpop",
        Command::RPop(_, _) => "rpop",
        Command::LRange(_, _, _) => "lrange",
        Command::LLen(_) => "llen",
        Command::LIndex(_, _) => "lindex",
        Command::LSet(_, _, _) => "lset",
        Command::LRem(_, _, _) => "lrem",
        Command::LTrim(_, _, _) => "ltrim",
        Command::SAdd(_, _) => "sadd",
        Command::SMembers(_) => "smembers",
        Command::SRem(_, _) => "srem",
        Command::SCard(_) => "scard",
        Command::SIsMember(_, _) => "sismember",
        Command::SMIsMember(_, _) => "smismember",
        Command::SInter(_) => "sinter",
        Command::SInterStore(_, _) => "sinterstore",
        Command::SUnion(_) => "sunion",
        Command::SUnionStore(_, _) => "sunionstore",
        Command::SDiff(_) => "sdiff",
        Command::SDiffStore(_, _) => "sdiffstore",
        Command::SPop(_, _) => "spop",
        Command::SRandMember(_, _) => "srandmember",
        Command::SMove(_, _, _) => "smove",
        Command::ZAdd(_, _, _) => "zadd",
        Command::ZRange(_, _, _, _) => "zrange",
        Command::ZRevRange(_, _, _, _) => "zrevrange",
        Command::ZRangeByScore(_, _, _, _, _) => "zrangebyscore",
        Command::ZRevRangeByScore(_, _, _, _, _) => "zrevrangebyscore",
        Command::ZScore(_, _) => "zscore",
        Command::ZMScore(_, _) => "zmscore",
        Command::ZRank(_, _) => "zrank",
        Command::ZRevRank(_, _) => "zrevrank",
        Command::ZRem(_, _) => "zrem",
        Command::ZCard(_) => "zcard",
        Command::ZIncrBy(_, _, _) => "zincrby",
        Command::ZCount(_, _, _) => "zcount",
        Command::Multi => "multi",
        Command::Exec => "exec",
        Command::Discard => "discard",
        Command::Subscribe(_) => "subscribe",
        Command::Unsubscribe(_) => "unsubscribe",
        Command::PSubscribe(_) => "psubscribe",
        Command::PUnsubscribe(_) => "punsubscribe",
        Command::Publish(_, _) => "publish",
        Command::Watch(_) => "watch",
        Command::Unwatch(_) => "unwatch",
        Command::Save => "save",
        Command::BgSave => "bgsave",
        Command::LastSave => "lastsave",
        Command::ReplicaOfNoOne => "replicaof",
        Command::JSet(_, _, _) => "jset",
        Command::JGet(_, _) => "jget",
        Command::JMerge(_, _) => "jmerge",
        Command::RlSet(_, _, _) => "rlset",
        Command::RlCheck(_, _) => "rlcheck",
        Command::Sync(_) => "sync",
        Command::QSub(_) => "qsub",
        Command::QUnsub(_) => "qunsub",
        // Metrics count the wrapped command, not the envelope.
        Command::Dedup(_, _, inner) => command_name(inner),
        Command::Unknown(_) => "unknown",
    }
}

/// Per-command counter handles, resolved through the metrics registry once and
/// then reused — the registry lookup (key construction + shard lock) is too
/// expensive to pay on every command. Keyed by the `&'static str` from
/// `command_name`. Populated lazily after the recorder is installed in `main`.
static CMD_COUNTERS: std::sync::LazyLock<
    std::sync::RwLock<HashMap<&'static str, metrics::Counter>>,
> = std::sync::LazyLock::new(Default::default);

fn record_command(name: &'static str) {
    if let Some(c) = CMD_COUNTERS.read().unwrap().get(name) {
        c.increment(1);
        return;
    }
    let c = counter!("recached_commands_total", "command" => name);
    c.increment(1);
    CMD_COUNTERS.write().unwrap().insert(name, c);
}

static KEYSPACE_HITS: std::sync::LazyLock<metrics::Counter> =
    std::sync::LazyLock::new(|| counter!("recached_keyspace_hits_total"));
static KEYSPACE_MISSES: std::sync::LazyLock<metrics::Counter> =
    std::sync::LazyLock::new(|| counter!("recached_keyspace_misses_total"));

/// Executes `cmd`, recording metrics and the dirty counter. Takes the command
/// by value — the hot path hands it straight to the store without a clone;
/// callers that still need the command afterwards (write fan-out) clone first.
fn execute_and_record(store: &KeyValueStore, cmd: Command) -> Value {
    let name = command_name(&cmd);
    let is_write = is_write_command(&cmd);
    let is_get = matches!(cmd, Command::Get(_));
    let response = store.execute(cmd);
    record_command(name);
    if matches!(response, Value::Error(_)) {
        counter!("recached_command_errors_total", "command" => name).increment(1);
    } else if is_write {
        store.mark_dirty();
    }
    if is_get {
        match &response {
            Value::BulkString(Some(_)) => KEYSPACE_HITS.increment(1),
            Value::BulkString(None) => KEYSPACE_MISSES.increment(1),
            _ => {}
        }
    }
    response
}

/// True when at least one consumer of write effects exists (WebSocket peers,
/// AOF, replicas, or watched keys). When false — the common standalone case —
/// the caller can move the command into `execute_and_record` without cloning
/// and skip `apply_write_effects` entirely.
fn write_effects_armed(
    tx: &broadcast::Sender<SyncMsg>,
    state: &ServerState,
    watch_registry: &WatchRegistry,
) -> bool {
    tx.receiver_count() > 0 || state.needs_write_log() || !watch_registry.is_empty()
}

// ── TCP listeners ─────────────────────────────────────────────────────────────

/// Binds `n` TCP sockets on `addr`, all with `SO_REUSEPORT`, so the OS can
/// distribute incoming connections across multiple accept loops — one per
/// Tokio worker thread. Falls back to a single plain `TcpListener::bind` on
/// platforms that don't support `SO_REUSEPORT`.
fn make_tcp_listeners(addr: &str, n: usize) -> std::io::Result<Vec<TcpListener>> {
    use socket2::{Domain, Socket, Type};
    let socket_addr: std::net::SocketAddr = addr
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let domain = if socket_addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    // SO_REUSEPORT — which lets multiple sockets share one port — is Unix-only.
    // Without it, binding a second socket to the same port fails, so fall back
    // to a single accept loop on non-Unix platforms.
    #[cfg(not(unix))]
    let n = 1;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let sock = Socket::new(domain, Type::STREAM, None)?;
        sock.set_reuse_address(true)?;
        #[cfg(unix)]
        sock.set_reuse_port(true)?;
        sock.set_nonblocking(true)?;
        sock.bind(&socket_addr.into())?;
        sock.listen(4096)?;
        let std_listener: std::net::TcpListener = sock.into();
        out.push(TcpListener::from_std(std_listener)?);
    }
    Ok(out)
}

// ── TLS ───────────────────────────────────────────────────────────────────────

fn load_certs(path: &str) -> std::io::Result<Vec<CertificateDer<'static>>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    certs(&mut reader).collect()
}

fn load_private_key(path: &str) -> std::io::Result<PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    private_key(&mut reader)?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no private key found"))
}

/// Returns a `TlsAcceptor` if both `RECACHED_TLS_CERT` and `RECACHED_TLS_KEY`
/// are set. Falls back to plain TCP when either is absent.
fn load_tls_acceptor() -> Option<TlsAcceptor> {
    let cert_path = std::env::var("RECACHED_TLS_CERT").ok()?;
    let key_path = std::env::var("RECACHED_TLS_KEY").ok()?;

    let cert_coll = load_certs(&cert_path).unwrap_or_else(|e| panic!("TLS cert {cert_path}: {e}"));
    let key = load_private_key(&key_path).unwrap_or_else(|e| panic!("TLS key {key_path}: {e}"));

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_coll, key)
        .expect("invalid TLS configuration");

    Some(TlsAcceptor::from(Arc::new(config)))
}

// ── tunables ────────────────────────────────────────────────────────────────

const TCP_READ_BUFFER_BYTES: usize = 16 * 1024; // 16 KB — matches Redis default
const MAX_TCP_READ_BUFFER_BYTES: usize = 64 * 1024 * 1024; // 64 MB per connection
const MAX_MULTI_QUEUE_LEN: usize = 10_000;
const MAX_WATCHES_PER_CONN: usize = 1_024;
const MAX_QSUBS_PER_CONN: usize = 64;
/// Cap on the number of key/value pairs returned as QSUB initial state, so a
/// pattern matching a huge keyspace cannot produce an unbounded reply frame.
const MAX_QSUB_INITIAL_KEYS: usize = 10_000;
const BROADCAST_CHANNEL_CAPACITY: usize = 512;
const DEFAULT_MAX_CONNECTIONS: usize = 1024;
const MAX_AUTH_FAILURES: u32 = 5;
const EVICTION_INTERVAL_SECS: u64 = 1;

// ── snapshot persistence ──────────────────────────────────────────────────────

struct SnapshotConfig {
    path: PathBuf,
    last_save: AtomicI64,
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Parse a human-readable memory size string (e.g. "512mb", "1gb", "262144")
/// into a byte count. Returns None on parse failure.
fn parse_memory_bytes(s: &str) -> Option<usize> {
    let s = s.trim().to_lowercase();
    if let Some(n) = s.strip_suffix("gb") {
        n.trim()
            .parse::<usize>()
            .ok()
            .map(|n| n * 1024 * 1024 * 1024)
    } else if let Some(n) = s.strip_suffix("mb") {
        n.trim().parse::<usize>().ok().map(|n| n * 1024 * 1024)
    } else if let Some(n) = s.strip_suffix("kb") {
        n.trim().parse::<usize>().ok().map(|n| n * 1024)
    } else {
        s.parse().ok()
    }
}

async fn save_snapshot(store: &KeyValueStore, cfg: &SnapshotConfig) {
    let entries = store.snapshot();
    let count = entries.len();
    let tmp = cfg.path.with_extension("tmp");
    match rmp_serde::to_vec(&entries) {
        Err(e) => warn!("Snapshot serialize failed: {}", e),
        Ok(bytes) => match tokio::fs::write(&tmp, &bytes).await {
            Err(e) => warn!("Snapshot write failed: {}", e),
            Ok(()) => match tokio::fs::rename(&tmp, &cfg.path).await {
                Err(e) => warn!("Snapshot rename failed: {}", e),
                Ok(()) => {
                    cfg.last_save.store(now_unix_secs(), Ordering::Relaxed);
                    info!("Snapshot saved: {} entries → {:?}", count, cfg.path);
                }
            },
        },
    }
}

async fn load_snapshot(store: &KeyValueStore, path: &std::path::Path) -> bool {
    match tokio::fs::read(path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!("No snapshot at {:?}, starting fresh", path);
            false
        }
        Err(e) => {
            warn!("Snapshot read failed: {}", e);
            false
        }
        Ok(bytes) => match rmp_serde::from_slice::<Vec<SnapshotEntry>>(&bytes) {
            Err(e) => {
                warn!("Snapshot deserialize failed: {}", e);
                false
            }
            Ok(entries) => {
                let count = entries.len();
                store.restore(entries);
                info!("Snapshot loaded: {} entries ← {:?}", count, path);
                true
            }
        },
    }
}

// ── AOF ───────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum AofSync {
    Always,
    EverySec,
    No,
}

struct AofWriter {
    #[allow(dead_code)]
    path: PathBuf,
    file: tokio::sync::Mutex<tokio::fs::File>,
    sync: AofSync,
}

impl AofWriter {
    async fn open(path: PathBuf, sync: AofSync) -> std::io::Result<Self> {
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        Ok(Self {
            path,
            file: tokio::sync::Mutex::new(file),
            sync,
        })
    }

    async fn append(&self, resp: &str) {
        let mut f = self.file.lock().await;
        if f.write_all(resp.as_bytes()).await.is_err() {
            warn!("AOF write failed");
            return;
        }
        if self.sync == AofSync::Always {
            let _ = f.flush().await;
        }
    }

    async fn flush(&self) {
        let _ = self.file.lock().await.flush().await;
    }

    async fn truncate(&self) {
        match self.file.lock().await.set_len(0).await {
            Ok(()) => info!("AOF truncated after snapshot save"),
            Err(e) => warn!("AOF truncate failed: {}", e),
        }
    }
}

async fn replay_aof(store: &KeyValueStore, path: &std::path::Path) -> usize {
    let bytes = match tokio::fs::read(path).await {
        Err(e) if e.kind() == ErrorKind::NotFound => return 0,
        Err(e) => {
            warn!("AOF read failed: {}", e);
            return 0;
        }
        Ok(b) => b,
    };
    let mut replayed = 0usize;
    let mut offset = 0;
    while offset < bytes.len() {
        match Value::parse(&bytes[offset..]) {
            Ok((value, consumed)) => {
                offset += consumed;
                // Writes are recorded via `on_write` in RESP3 Push form (`>N`);
                // normalise to Array so Command::from_value can parse them.
                let normalised = match value {
                    Value::Push(inner) => Value::Array(Some(inner)),
                    other => other,
                };
                if let Ok(cmd) = Command::from_value(normalised) {
                    store.execute(cmd);
                    replayed += 1;
                }
            }
            Err(ref e) if e == "Incomplete" => break,
            Err(_) => {
                warn!("AOF corrupted at offset {}, stopping replay", offset);
                break;
            }
        }
    }
    if replayed > 0 {
        info!("AOF replayed: {} commands ← {:?}", replayed, path);
    }
    replayed
}

// ── Replication ───────────────────────────────────────────────────────────────

type ReplSender = mpsc::Sender<Vec<u8>>;

/// Connected-replica registry. `count` mirrors `senders.len()` (updated by
/// every writer while holding the lock) so the per-write hot path can skip
/// the mutex entirely when no replica is connected.
struct ReplHub {
    senders: tokio::sync::Mutex<Vec<ReplSender>>,
    count: AtomicUsize,
}

impl ReplHub {
    fn new() -> ReplRegistry {
        Arc::new(ReplHub {
            senders: tokio::sync::Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
        })
    }

    fn is_empty(&self) -> bool {
        self.count.load(Ordering::Relaxed) == 0
    }
}

type ReplRegistry = Arc<ReplHub>;

/// Default per-replica channel capacity (number of pending write frames).
/// When a replica falls this many writes behind the primary it is disconnected
/// so it can reconnect and receive a fresh snapshot — the primary write path
/// is never blocked.
const DEFAULT_REPL_CHANNEL_CAPACITY: usize = 4096;

/// Upper bound on a single length-prefixed replication frame (snapshot or
/// command). The replication port may be unauthenticated and plaintext, so an
/// untrusted peer could otherwise send a 4 GB length prefix and force a matching
/// allocation. 512 MB comfortably covers a large snapshot while bounding abuse.
const MAX_REPL_FRAME_BYTES: usize = 512 * 1024 * 1024;

// ── Server state ──────────────────────────────────────────────────────────────

struct ServerState {
    snap: Arc<SnapshotConfig>,
    aof: Option<Arc<AofWriter>>,
    replicas: ReplRegistry,
    /// true = currently acting as a read-only replica
    is_replica: std::sync::atomic::AtomicBool,
    /// Exactly-once bookkeeping for DEDUP-wrapped writes: client id →
    /// (highest id applied, last-seen ms). Clients send monotonically
    /// increasing ids and replay in order, so a single high-water mark per
    /// client suffices — no seen-set. In-memory only: a server restart
    /// reopens the (already narrow) duplicate window, which is documented.
    dedup: std::sync::Mutex<HashMap<String, (u64, u64)>>,
}

/// Sweep dedup client entries idle longer than this once the map is large.
const DEDUP_IDLE_MS: u64 = 24 * 60 * 60 * 1000;
const DEDUP_SWEEP_THRESHOLD: usize = 10_000;

impl ServerState {
    fn is_replica(&self) -> bool {
        self.is_replica.load(Ordering::Relaxed)
    }

    fn promote_to_primary(&self) {
        self.is_replica.store(false, Ordering::Relaxed);
        info!("REPLICAOF NO ONE: promoted to primary — writes now accepted");
    }

    /// True when a write must be RESP-encoded for the durability/replication
    /// path even if no other consumer needs it.
    fn needs_write_log(&self) -> bool {
        self.aof.is_some() || !self.replicas.is_empty()
    }

    /// Record a DEDUP-wrapped write. Returns `true` when `id` was already
    /// applied for this client (the write must be skipped). Marks the id
    /// *before* execution so a crash between check and execute can never
    /// double-apply.
    fn dedup_seen(&self, client: &str, id: u64) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut map = self.dedup.lock().expect("dedup mutex poisoned");
        if map.len() > DEDUP_SWEEP_THRESHOLD {
            map.retain(|_, (_, seen)| now.saturating_sub(*seen) < DEDUP_IDLE_MS);
        }
        match map.get_mut(client) {
            Some((hwm, seen)) => {
                *seen = now;
                if id <= *hwm {
                    true
                } else {
                    *hwm = id;
                    false
                }
            }
            None => {
                map.insert(client.to_string(), (id, now));
                false
            }
        }
    }

    /// Called after every successful write: appends to AOF and fans out to replicas.
    async fn on_write(&self, resp: &str) {
        if let Some(aof) = &self.aof {
            aof.append(resp).await;
        }
        if self.replicas.is_empty() {
            return;
        }
        let bytes = resp.as_bytes().to_vec();
        let mut reg = self.replicas.senders.lock().await;
        reg.retain(|tx| match tx.try_send(bytes.clone()) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    "Replica fell too far behind (channel full) — disconnecting so it can resync"
                );
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        });
        self.replicas.count.store(reg.len(), Ordering::Relaxed);
    }

    /// Save snapshot, reset the dirty counter, then truncate AOF (snapshot subsumes the log).
    async fn save(&self, store: &KeyValueStore) {
        save_snapshot(store, &self.snap).await;
        store.reset_dirty();
        if let Some(aof) = &self.aof {
            aof.truncate().await;
        }
    }
}

fn is_write_command(cmd: &Command) -> bool {
    if let Command::Dedup(_, _, inner) = cmd {
        return is_write_command(inner);
    }
    matches!(
        cmd,
        Command::Set(..)
            | Command::Del(..)
            | Command::Unlink(..)
            | Command::Append(..)
            | Command::GetSet(..)
            | Command::MSet(..)
            | Command::SetNx(..)
            | Command::SetEx(..)
            | Command::PSetEx(..)
            | Command::Incr(..)
            | Command::Decr(..)
            | Command::IncrBy(..)
            | Command::DecrBy(..)
            | Command::Expire(..)
            | Command::PExpire(..)
            | Command::ExpireAt(..)
            | Command::PExpireAt(..)
            | Command::Persist(..)
            | Command::FlushDb
            | Command::Rename(..)
            | Command::HSet(..)
            | Command::HDel(..)
            | Command::HIncrBy(..)
            | Command::HIncrByFloat(..)
            | Command::HSetNx(..)
            | Command::LPush(..)
            | Command::RPush(..)
            | Command::LPushX(..)
            | Command::RPushX(..)
            | Command::LPop(..)
            | Command::RPop(..)
            | Command::LSet(..)
            | Command::LRem(..)
            | Command::LTrim(..)
            | Command::SAdd(..)
            | Command::SRem(..)
            | Command::SInterStore(..)
            | Command::SUnionStore(..)
            | Command::SDiffStore(..)
            | Command::SPop(..)
            | Command::SMove(..)
            | Command::ZAdd(..)
            | Command::ZRem(..)
            | Command::ZIncrBy(..)
            | Command::RlSet(..)
            | Command::RlCheck(..)
            | Command::JSet(..)
            | Command::JMerge(..)
    )
}

// ── Save conditions ───────────────────────────────────────────────────────────

/// A single autosave condition: save if `changes` or more writes have
/// accumulated within `secs` seconds of the last save.
struct SaveCondition {
    secs: u64,
    changes: u64,
}

/// Parse `RECACHED_SAVE` value: comma-separated `seconds:changes` pairs.
/// Example: `"900:1,300:10,60:10000"` → save after 1 change in 15 min,
/// 10 changes in 5 min, or 10 000 changes in 1 min — whichever comes first.
fn parse_save_conditions(s: &str) -> Vec<SaveCondition> {
    s.split(',')
        .filter_map(|pair| {
            let mut parts = pair.trim().splitn(2, ':');
            let secs: u64 = parts.next()?.trim().parse().ok()?;
            let changes: u64 = parts.next()?.trim().parse().ok()?;
            Some(SaveCondition { secs, changes })
        })
        .collect()
}

// ── Replication server (primary side) ────────────────────────────────────────

async fn run_repl_server(
    bind_host: String,
    port: u16,
    store: Arc<KeyValueStore>,
    snap_cfg: Arc<SnapshotConfig>,
    replicas: ReplRegistry,
    repl_password: Option<Arc<String>>,
    repl_channel_capacity: usize,
) {
    let listener = match TcpListener::bind(format!("{}:{}", bind_host, port)).await {
        Ok(l) => l,
        Err(e) => {
            warn!("Replication listener failed to bind :{}: {}", port, e);
            return;
        }
    };
    info!("Replication server listening on {}:{}", bind_host, port);
    loop {
        match listener.accept().await {
            Ok((socket, addr)) => {
                info!("Replica connected from {}", addr);
                let store = Arc::clone(&store);
                let snap_cfg = Arc::clone(&snap_cfg);
                let replicas = Arc::clone(&replicas);
                let pwd = repl_password.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_replica(
                        socket,
                        store,
                        snap_cfg,
                        replicas,
                        pwd,
                        repl_channel_capacity,
                    )
                    .await
                    {
                        info!("Replica {} disconnected: {}", addr, e);
                    }
                });
            }
            Err(e) => warn!("Replication accept error: {}", e),
        }
    }
}

async fn handle_replica(
    mut socket: TcpStream,
    store: Arc<KeyValueStore>,
    _snap_cfg: Arc<SnapshotConfig>,
    replicas: ReplRegistry,
    repl_password: Option<Arc<String>>,
    repl_channel_capacity: usize,
) -> std::io::Result<()> {
    // 0. Auth handshake — replica must send "<password>\n" before anything else
    if let Some(pwd) = &repl_password {
        let mut auth_buf = vec![0u8; pwd.len() + 1];
        socket.read_exact(&mut auth_buf).await?;
        let received_pwd = &auth_buf[..pwd.len()];
        let terminator = auth_buf[pwd.len()];
        if !ct_eq_bytes(received_pwd, pwd.as_bytes()) || terminator != b'\n' {
            let _ = socket
                .write_all(b"-ERR invalid replication password\n")
                .await;
            return Err(std::io::Error::new(
                ErrorKind::PermissionDenied,
                "replication auth failed",
            ));
        }
        socket.write_all(b"+OK\n").await?;
        socket.flush().await?;
    }

    // 1. Register channel first so subsequent writes are buffered
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(repl_channel_capacity);
    {
        let mut reg = replicas.senders.lock().await;
        reg.push(tx);
        replicas.count.store(reg.len(), Ordering::Relaxed);
    }

    // 2. Take snapshot and send (writes since snapshot are in channel)
    let snap_bytes =
        rmp_serde::to_vec(&store.snapshot()).map_err(|e| std::io::Error::other(e.to_string()))?;
    let len = snap_bytes.len() as u32;
    socket.write_all(&len.to_le_bytes()).await?;
    socket.write_all(&snap_bytes).await?;
    socket.flush().await?;

    // 3. Stream buffered + ongoing writes
    while let Some(bytes) = rx.recv().await {
        let len = bytes.len() as u32;
        socket.write_all(&len.to_le_bytes()).await?;
        socket.write_all(&bytes).await?;
        socket.flush().await?;
    }
    Ok(())
}

// ── Replication client (replica side) ────────────────────────────────────────

async fn run_repl_client(
    primary_addr: String,
    store: Arc<KeyValueStore>,
    state: Arc<ServerState>,
    repl_password: Option<String>,
    failover_timeout_secs: Option<u64>,
    tx: broadcast::Sender<SyncMsg>,
) {
    let mut backoff_secs = 2u64;
    let mut unreachable_since: Option<std::time::Instant> = None;

    loop {
        // Stop if already promoted (manual REPLICAOF NO ONE or earlier auto-promotion).
        if !state.is_replica() {
            return;
        }

        info!("Replica: connecting to primary at {}", primary_addr);
        match TcpStream::connect(&primary_addr).await {
            Err(e) => {
                warn!("Replica: connect failed: {}", e);
                unreachable_since.get_or_insert_with(std::time::Instant::now);
            }
            Ok(mut socket) => {
                // Primary is reachable — reset the unreachable timer.
                unreachable_since = None;
                backoff_secs = 2;
                if let Err(e) =
                    sync_from_primary(&mut socket, &store, repl_password.as_deref(), &tx, &state)
                        .await
                {
                    warn!("Replica: sync ended: {}", e);
                    // Sync dropped — primary may be gone; start tracking if not already.
                    unreachable_since.get_or_insert_with(std::time::Instant::now);
                }
            }
        }

        // Auto-failover: promote if primary has been unreachable long enough.
        if let (Some(timeout), Some(since)) = (failover_timeout_secs, unreachable_since) {
            let elapsed = since.elapsed().as_secs();
            if elapsed >= timeout {
                warn!(
                    "Replica: primary unreachable for {}s (timeout {}s) — auto-promoting to primary",
                    elapsed, timeout
                );
                state.promote_to_primary();
                return;
            }
            info!(
                "Replica: primary unreachable for {}s / {}s before auto-failover",
                elapsed, timeout
            );
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(30);
    }
}

async fn sync_from_primary(
    socket: &mut TcpStream,
    store: &KeyValueStore,
    repl_password: Option<&str>,
    tx: &broadcast::Sender<SyncMsg>,
    state: &ServerState,
) -> std::io::Result<()> {
    // 0. Send auth password if configured
    if let Some(pwd) = repl_password {
        let msg = format!("{}\n", pwd);
        socket.write_all(msg.as_bytes()).await?;
        socket.flush().await?;
        // Read "+OK\n" (4 bytes)
        let mut resp = [0u8; 4];
        socket.read_exact(&mut resp).await?;
        if &resp != b"+OK\n" {
            return Err(std::io::Error::new(
                ErrorKind::PermissionDenied,
                "replication auth rejected by primary",
            ));
        }
    }

    // 1. Receive full snapshot
    let mut len_buf = [0u8; 4];
    socket.read_exact(&mut len_buf).await?;
    let snap_len = u32::from_le_bytes(len_buf) as usize;
    if snap_len > MAX_REPL_FRAME_BYTES {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            format!("snapshot frame too large ({snap_len} > {MAX_REPL_FRAME_BYTES} bytes)"),
        ));
    }
    let mut snap_bytes = vec![0u8; snap_len];
    socket.read_exact(&mut snap_bytes).await?;

    match rmp_serde::from_slice::<Vec<SnapshotEntry>>(&snap_bytes) {
        Ok(entries) => {
            let count = entries.len();
            store.restore(entries);
            info!("Replica: snapshot loaded ({} entries)", count);
        }
        Err(e) => {
            return Err(std::io::Error::new(ErrorKind::InvalidData, e.to_string()));
        }
    }

    // 2. Stream write commands from primary
    loop {
        let mut len_buf = [0u8; 4];
        socket.read_exact(&mut len_buf).await?;
        let cmd_len = u32::from_le_bytes(len_buf) as usize;
        if cmd_len > MAX_REPL_FRAME_BYTES {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                format!("command frame too large ({cmd_len} > {MAX_REPL_FRAME_BYTES} bytes)"),
            ));
        }
        let mut cmd_bytes = vec![0u8; cmd_len];
        socket.read_exact(&mut cmd_bytes).await?;

        match Value::parse(&cmd_bytes) {
            Ok((value, _)) => {
                // Replication frames are broadcast as RESP3 Push (>N\r\n); normalise to
                // Array so Command::from_value can parse them.
                let normalised = match value {
                    Value::Push(inner) => Value::Array(Some(inner)),
                    other => other,
                };
                if let Ok(cmd) = Command::from_value(normalised) {
                    let keys = primary_keys(&cmd);
                    store.execute(cmd);
                    // Relay the applied write so this replica's own WebSocket
                    // clients see it, and any sub-replicas / AOF get it too
                    // (enables multi-tier replication and replica WS push).
                    let frame = String::from_utf8_lossy(&cmd_bytes).into_owned();
                    let _ = tx.send(Arc::new(SyncPush {
                        origin: 0,
                        keys,
                        resp: frame.clone(),
                    }));
                    state.on_write(&frame).await;
                }
            }
            Err(e) => warn!("Replica: bad command from primary: {}", e),
        }
    }
}

// ── security helpers ─────────────────────────────────────────────────────────

/// Constant-time byte slice equality to prevent timing-based password leaks.
fn ct_eq_bytes(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

// ── sync scoping ─────────────────────────────────────────────────────────────

/// One mutation pushed towards WebSocket peers: the RESP push frame plus the
/// keys it touches, so each connection can filter against its sync scopes
/// without re-parsing the frame. Wrapped in `Arc` — the broadcast channel
/// clones the payload once per receiver, so a clone is a refcount bump.
struct SyncPush {
    origin: u64,
    keys: Vec<String>,
    resp: String,
}

type SyncMsg = Arc<SyncPush>;

/// True when a mutation touching `keys` is visible to a connection whose sync
/// scopes are `scopes`. A mutation with no keys (FLUSHDB) affects every scope.
fn scopes_match(scopes: &[String], keys: &[String]) -> bool {
    keys.is_empty()
        || keys
            .iter()
            .any(|k| scopes.iter().any(|p| core_engine::store::glob_match(p, k)))
}

/// Verify a signed sync-scope token and return the granted patterns.
///
/// Token format: `base64url(payload) "." base64url(hmac_sha256(secret, base64url(payload)))`
/// where payload is comma-separated glob patterns with an optional
/// `|<unix_expiry_secs>` suffix. The HMAC is computed over the *encoded*
/// payload string, so minting in JS is one `createHmac` call on the base64url
/// text — no byte-level canonicalisation questions.
fn verify_sync_token(secret: &str, token: &str) -> Result<Vec<String>, &'static str> {
    use base64::Engine as _;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let (payload_b64, sig_b64) = token.split_once('.').ok_or("malformed token")?;
    let sig = engine.decode(sig_b64).map_err(|_| "malformed signature")?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(payload_b64.as_bytes());
    if !ct_eq_bytes(&sig, &mac.finalize().into_bytes()) {
        return Err("invalid signature");
    }
    let payload_bytes = engine
        .decode(payload_b64)
        .map_err(|_| "malformed payload")?;
    let payload = String::from_utf8(payload_bytes).map_err(|_| "malformed payload")?;
    let (patterns_str, expiry) = match payload.split_once('|') {
        Some((p, e)) => (p, Some(e)),
        None => (payload.as_str(), None),
    };
    if let Some(e) = expiry {
        let exp: u64 = e.parse().map_err(|_| "malformed expiry")?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now >= exp {
            return Err("token expired");
        }
    }
    let patterns: Vec<String> = patterns_str
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if patterns.is_empty() {
        return Err("token grants no patterns");
    }
    Ok(patterns)
}

/// What a command touches, for scope enforcement on token-scoped WebSocket
/// connections.
enum CommandScope {
    /// No key access (PING, AUTH, MULTI, SYNC, pub/sub) — always allowed.
    KeyLess,
    /// Touches exactly these keys — every one must match a scope pattern.
    Keys(Vec<String>),
    /// Keyspace-wide or administrative — denied on scoped connections.
    Admin,
}

fn command_scope(cmd: &Command) -> CommandScope {
    match cmd {
        Command::Ping(_)
        | Command::Auth(_)
        | Command::Multi
        | Command::Exec
        | Command::Discard
        | Command::Subscribe(_)
        | Command::Unsubscribe(_)
        | Command::PSubscribe(_)
        | Command::PUnsubscribe(_)
        | Command::Publish(_, _)
        | Command::Sync(_)
        // QSUB patterns are scope-checked against the grant in the WS handler.
        | Command::QSub(_)
        | Command::QUnsub(_)
        | Command::Unknown(_) => CommandScope::KeyLess,

        Command::Keys(_)
        | Command::Scan(_, _, _)
        | Command::DbSize
        | Command::FlushDb
        | Command::Save
        | Command::BgSave
        | Command::LastSave
        | Command::ReplicaOfNoOne => CommandScope::Admin,

        Command::Set(k, _, _)
        | Command::Get(k)
        | Command::Append(k, _)
        | Command::Strlen(k)
        | Command::GetSet(k, _)
        | Command::SetNx(k, _)
        | Command::SetEx(k, _, _)
        | Command::PSetEx(k, _, _)
        | Command::Incr(k)
        | Command::Decr(k)
        | Command::IncrBy(k, _)
        | Command::DecrBy(k, _)
        | Command::Expire(k, _)
        | Command::PExpire(k, _)
        | Command::ExpireAt(k, _)
        | Command::PExpireAt(k, _)
        | Command::Ttl(k)
        | Command::PTtl(k)
        | Command::Persist(k)
        | Command::Type(k)
        | Command::HSet(k, _)
        | Command::HGet(k, _)
        | Command::HGetAll(k)
        | Command::HDel(k, _)
        | Command::HKeys(k)
        | Command::HVals(k)
        | Command::HLen(k)
        | Command::HIncrBy(k, _, _)
        | Command::HIncrByFloat(k, _, _)
        | Command::HExists(k, _)
        | Command::HSetNx(k, _, _)
        | Command::HMGet(k, _)
        | Command::LPush(k, _)
        | Command::RPush(k, _)
        | Command::LPushX(k, _)
        | Command::RPushX(k, _)
        | Command::LPop(k, _)
        | Command::RPop(k, _)
        | Command::LRange(k, _, _)
        | Command::LLen(k)
        | Command::LIndex(k, _)
        | Command::LSet(k, _, _)
        | Command::LRem(k, _, _)
        | Command::LTrim(k, _, _)
        | Command::SAdd(k, _)
        | Command::SMembers(k)
        | Command::SRem(k, _)
        | Command::SCard(k)
        | Command::SIsMember(k, _)
        | Command::SMIsMember(k, _)
        | Command::SPop(k, _)
        | Command::SRandMember(k, _)
        | Command::ZAdd(k, _, _)
        | Command::ZRange(k, _, _, _)
        | Command::ZRevRange(k, _, _, _)
        | Command::ZRangeByScore(k, _, _, _, _)
        | Command::ZRevRangeByScore(k, _, _, _, _)
        | Command::ZScore(k, _)
        | Command::ZMScore(k, _)
        | Command::ZRank(k, _)
        | Command::ZRevRank(k, _)
        | Command::ZRem(k, _)
        | Command::ZCard(k)
        | Command::ZIncrBy(k, _, _)
        | Command::ZCount(k, _, _)
        | Command::RlSet(k, _, _)
        | Command::RlCheck(k, _)
        | Command::JSet(k, _, _)
        | Command::JGet(k, _)
        | Command::JMerge(k, _) => CommandScope::Keys(vec![k.clone()]),

        Command::Del(keys)
        | Command::Unlink(keys)
        | Command::MGet(keys)
        | Command::Exists(keys)
        | Command::SInter(keys)
        | Command::SUnion(keys)
        | Command::SDiff(keys)
        | Command::Watch(keys)
        | Command::Unwatch(keys) => CommandScope::Keys(keys.clone()),

        Command::MSet(pairs) => CommandScope::Keys(pairs.iter().map(|(k, _)| k.clone()).collect()),
        Command::Rename(src, dst) | Command::SMove(src, dst, _) => {
            CommandScope::Keys(vec![src.clone(), dst.clone()])
        }
        Command::SInterStore(dst, keys)
        | Command::SUnionStore(dst, keys)
        | Command::SDiffStore(dst, keys) => {
            let mut all = keys.clone();
            all.push(dst.clone());
            CommandScope::Keys(all)
        }

        // Scope enforcement applies to the wrapped command.
        Command::Dedup(_, _, inner) => command_scope(inner),
    }
}

/// Handle the SYNC command for one WebSocket connection, returning the RESP
/// reply. Forms:
///   `SYNC`                 — list this connection's current scopes
///   `SYNC TOKEN <token>`   — set scopes from a signed token (requires
///                            `RECACHED_SYNC_SECRET` on the server)
///   `SYNC <pattern> [...]` — set scopes directly (only allowed when no
///                            secret is configured — a bandwidth filter, not
///                            an authorization boundary)
fn handle_sync_command(
    args: &[String],
    secret: Option<&str>,
    scopes: &mut Option<Vec<String>>,
    conn_id: u64,
) -> Vec<u8> {
    fn patterns_reply(patterns: &[String]) -> Vec<u8> {
        Value::Array(Some(
            patterns
                .iter()
                .map(|p| Value::BulkString(Some(p.clone().into_bytes())))
                .collect(),
        ))
        .serialize()
    }
    match args {
        [] => patterns_reply(scopes.as_deref().unwrap_or(&[])),
        [kw, token] if kw.eq_ignore_ascii_case("token") => {
            let Some(secret) = secret else {
                return b"-ERR SYNC TOKEN requires RECACHED_SYNC_SECRET to be configured on the server\r\n"
                    .to_vec();
            };
            match verify_sync_token(secret, token) {
                Ok(patterns) => {
                    info!("WS conn {} scoped via token: {:?}", conn_id, patterns);
                    let reply = patterns_reply(&patterns);
                    *scopes = Some(patterns);
                    reply
                }
                Err(e) => Value::Error(format!("ERR invalid sync token: {}", e)).serialize(),
            }
        }
        patterns => {
            if secret.is_some() {
                return b"-ERR this server requires signed scopes: use SYNC TOKEN <token>\r\n"
                    .to_vec();
            }
            let pats: Vec<String> = patterns.iter().filter(|p| !p.is_empty()).cloned().collect();
            if pats.is_empty() {
                return b"-ERR SYNC requires at least one pattern\r\n".to_vec();
            }
            info!("WS conn {} sync scopes set: {:?}", conn_id, pats);
            let reply = patterns_reply(&pats);
            *scopes = Some(pats);
            reply
        }
    }
}

// ── connection identity ──────────────────────────────────────────────────────

// TCP mutation broadcasts use id=0; WS/TCP pubsub connections get ids ≥ 1.
static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

fn next_conn_id() -> u64 {
    NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed)
}

// ── pub/sub ───────────────────────────────────────────────────────────────────

enum PubSubMsg {
    Message {
        channel: String,
        message: String,
    },
    PMessage {
        pattern: String,
        channel: String,
        message: String,
    },
}

type PubSubSender = mpsc::UnboundedSender<PubSubMsg>;

struct PubSubHub {
    channel_subs: HashMap<String, Vec<(u64, PubSubSender)>>,
    pattern_subs: Vec<(String, u64, PubSubSender)>,
}

impl PubSubHub {
    fn new() -> Self {
        Self {
            channel_subs: HashMap::new(),
            pattern_subs: Vec::new(),
        }
    }

    fn subscribe(&mut self, conn_id: u64, channel: &str, tx: PubSubSender) {
        self.channel_subs
            .entry(channel.to_string())
            .or_default()
            .push((conn_id, tx));
    }

    fn psubscribe(&mut self, conn_id: u64, pattern: &str, tx: PubSubSender) {
        self.pattern_subs.push((pattern.to_string(), conn_id, tx));
    }

    fn unsubscribe(&mut self, conn_id: u64, channel: &str) {
        if let Some(v) = self.channel_subs.get_mut(channel) {
            v.retain(|(id, _)| *id != conn_id);
            if v.is_empty() {
                self.channel_subs.remove(channel);
            }
        }
    }

    fn punsubscribe(&mut self, conn_id: u64, pattern: &str) {
        self.pattern_subs
            .retain(|(p, id, _)| !(p == pattern && *id == conn_id));
    }

    fn unsubscribe_all(&mut self, conn_id: u64) {
        self.channel_subs.retain(|_, v| {
            v.retain(|(id, _)| *id != conn_id);
            !v.is_empty()
        });
        self.pattern_subs.retain(|(_, id, _)| *id != conn_id);
    }

    /// Deliver to all matching subscribers; returns the count delivered.
    fn publish(&mut self, channel: &str, message: &str) -> i64 {
        let mut count = 0i64;

        if let std::collections::hash_map::Entry::Occupied(mut e) =
            self.channel_subs.entry(channel.to_string())
        {
            let subs = e.get_mut();
            subs.retain(|(_, tx)| {
                let ok = tx
                    .send(PubSubMsg::Message {
                        channel: channel.to_string(),
                        message: message.to_string(),
                    })
                    .is_ok();
                if ok {
                    count += 1;
                }
                ok
            });
            if subs.is_empty() {
                e.remove();
            }
        }

        let pattern_txs: Vec<(String, PubSubSender)> = self
            .pattern_subs
            .iter()
            .filter(|(p, _, _)| glob_match(p, channel))
            .map(|(p, _, tx)| (p.clone(), tx.clone()))
            .collect();
        for (pattern, tx) in pattern_txs {
            if tx
                .send(PubSubMsg::PMessage {
                    pattern,
                    channel: channel.to_string(),
                    message: message.to_string(),
                })
                .is_ok()
            {
                count += 1;
            }
        }
        self.pattern_subs.retain(|(_, _, tx)| !tx.is_closed());
        count
    }
}

type SharedPubSub = Arc<tokio::sync::Mutex<PubSubHub>>;

// ── observable keys ───────────────────────────────────────────────────────────

type WatchNotif = (String, Value);
type WatchMap = HashMap<String, Vec<(u64, mpsc::UnboundedSender<WatchNotif>)>>;

/// Watched-key and live-query registry. `watched_keys` / `watched_patterns`
/// mirror the map lengths (updated by every writer while holding the lock) so
/// the per-write hot path can skip the mutexes entirely when nothing is
/// watched.
struct WatchHub {
    /// Exact-key watchers (WATCH).
    map: tokio::sync::Mutex<WatchMap>,
    watched_keys: AtomicUsize,
    /// Glob-pattern subscribers (QSUB live queries), keyed by pattern.
    patterns: tokio::sync::Mutex<WatchMap>,
    watched_patterns: AtomicUsize,
}

impl WatchHub {
    fn new() -> WatchRegistry {
        Arc::new(WatchHub {
            map: tokio::sync::Mutex::new(HashMap::new()),
            watched_keys: AtomicUsize::new(0),
            patterns: tokio::sync::Mutex::new(HashMap::new()),
            watched_patterns: AtomicUsize::new(0),
        })
    }

    fn is_empty(&self) -> bool {
        self.watched_keys.load(Ordering::Relaxed) == 0
            && self.watched_patterns.load(Ordering::Relaxed) == 0
    }

    /// Call after mutating the key map, while still holding the lock.
    fn sync_len(&self, map: &WatchMap) {
        self.watched_keys.store(map.len(), Ordering::Relaxed);
    }

    /// Call after mutating the pattern map, while still holding the lock.
    fn sync_patterns_len(&self, map: &WatchMap) {
        self.watched_patterns.store(map.len(), Ordering::Relaxed);
    }
}

type WatchRegistry = Arc<WatchHub>;

/// Extract the key(s) that `cmd` writes to, without inspecting the response.
/// Used together with `broadcast_for()` — only call this when `broadcast_for`
/// already confirmed a mutation occurred.
fn primary_keys(cmd: &Command) -> Vec<String> {
    match cmd {
        Command::Set(k, _, _)
        | Command::Append(k, _)
        | Command::GetSet(k, _)
        | Command::SetNx(k, _)
        | Command::SetEx(k, _, _)
        | Command::PSetEx(k, _, _)
        | Command::Incr(k)
        | Command::Decr(k)
        | Command::IncrBy(k, _)
        | Command::DecrBy(k, _)
        | Command::Expire(k, _)
        | Command::PExpire(k, _)
        | Command::ExpireAt(k, _)
        | Command::PExpireAt(k, _)
        | Command::Persist(k)
        | Command::HSet(k, _)
        | Command::HDel(k, _)
        | Command::HSetNx(k, _, _)
        | Command::HIncrBy(k, _, _)
        | Command::HIncrByFloat(k, _, _)
        | Command::LPush(k, _)
        | Command::RPush(k, _)
        | Command::LPushX(k, _)
        | Command::RPushX(k, _)
        | Command::LPop(k, _)
        | Command::RPop(k, _)
        | Command::LSet(k, _, _)
        | Command::LRem(k, _, _)
        | Command::LTrim(k, _, _)
        | Command::SAdd(k, _)
        | Command::SRem(k, _)
        | Command::SPop(k, _)
        | Command::SInterStore(k, _)
        | Command::SUnionStore(k, _)
        | Command::SDiffStore(k, _)
        | Command::ZAdd(k, _, _)
        | Command::ZRem(k, _)
        | Command::ZIncrBy(k, _, _)
        | Command::RlSet(k, _, _)
        | Command::JSet(k, _, _)
        | Command::JMerge(k, _) => vec![k.clone()],
        Command::Del(keys) | Command::Unlink(keys) => keys.clone(),
        Command::MSet(pairs) => pairs.iter().map(|(k, _)| k.clone()).collect(),
        Command::Rename(src, dst) | Command::SMove(src, dst, _) => {
            vec![src.clone(), dst.clone()]
        }
        _ => vec![],
    }
}

fn encode_keychange(key: &str, value: &Value) -> Vec<u8> {
    Value::Array(Some(vec![
        Value::BulkString(Some(b"keychange".to_vec())),
        Value::BulkString(Some(key.as_bytes().to_vec())),
        value.clone(),
    ]))
    .serialize()
}

/// Push keychange notifications for a *confirmed* mutation. Callers must have
/// already established that `cmd` mutated the store (via `broadcast_for`).
async fn notify_watchers(registry: &WatchRegistry, cmd: &Command, store: &KeyValueStore) {
    if registry.is_empty() {
        return;
    }
    let keys = primary_keys(cmd);
    if keys.is_empty() {
        return;
    }
    // Fetch current values from DashMap *before* acquiring the registry lock
    // to avoid holding two locks simultaneously.
    let key_values: Vec<(String, Value)> = keys
        .iter()
        .map(|k| (k.clone(), store.get_current(k)))
        .collect();
    if registry.watched_keys.load(Ordering::Relaxed) > 0 {
        let mut reg = registry.map.lock().await;
        for (key, value) in &key_values {
            if let Some(subs) = reg.get_mut(key) {
                subs.retain(|(_, tx)| tx.send((key.clone(), value.clone())).is_ok());
                if subs.is_empty() {
                    reg.remove(key);
                }
            }
        }
        registry.sync_len(&reg);
    }
    // Live queries: any registered glob pattern matching a touched key gets
    // the same keychange notification.
    if registry.watched_patterns.load(Ordering::Relaxed) > 0 {
        let mut pats = registry.patterns.lock().await;
        let mut emptied = false;
        for (pattern, subs) in pats.iter_mut() {
            for (key, value) in &key_values {
                if core_engine::store::glob_match(pattern, key) {
                    subs.retain(|(_, tx)| tx.send((key.clone(), value.clone())).is_ok());
                }
            }
            emptied |= subs.is_empty();
        }
        if emptied {
            pats.retain(|_, subs| !subs.is_empty());
        }
        registry.sync_patterns_len(&pats);
    }
}

/// Drop all of `conn_id`'s live-query subscriptions. Called on QUNSUB (all
/// form) and on connection close.
async fn unregister_all_qsubs(
    registry: &WatchRegistry,
    conn_id: u64,
    qsub_patterns: &mut HashSet<String>,
) {
    if qsub_patterns.is_empty() {
        return;
    }
    let mut pats = registry.patterns.lock().await;
    for p in qsub_patterns.drain() {
        if let Some(subs) = pats.get_mut(&p) {
            subs.retain(|(id, _)| *id != conn_id);
            if subs.is_empty() {
                pats.remove(&p);
            }
        }
    }
    registry.sync_patterns_len(&pats);
}

/// Post-write fan-out shared by the TCP and WS command paths: WebSocket sync
/// broadcast, AOF/replication log, and watch notifications. Structured so that
/// with no WS clients, no replicas, no AOF, and no watched keys — the common
/// standalone-server case — a write costs zero locks and zero allocations here.
async fn apply_write_effects(
    cmd: &Command,
    response: &Value,
    tx: &broadcast::Sender<SyncMsg>,
    origin: u64,
    state: &ServerState,
    watch_registry: &WatchRegistry,
    store: &KeyValueStore,
) {
    let has_ws = tx.receiver_count() > 0;
    let needs_log = state.needs_write_log();
    let has_watch = !watch_registry.is_empty();
    if !has_ws && !needs_log && !has_watch {
        return;
    }
    let Some(msg) = broadcast_for(cmd, response) else {
        return;
    };
    if needs_log {
        state.on_write(&msg).await;
    }
    if has_watch {
        notify_watchers(watch_registry, cmd, store).await;
    }
    if has_ws {
        let _ = tx.send(Arc::new(SyncPush {
            origin,
            keys: primary_keys(cmd),
            resp: msg,
        }));
    }
}

/// Drop all of `conn_id`'s WATCH registrations and clear `watched_keys`.
/// Called at every transaction boundary (EXEC, DISCARD) and on connection close,
/// matching Redis semantics that WATCH state is flushed by EXEC/DISCARD.
async fn unregister_all_watches(
    registry: &WatchRegistry,
    conn_id: u64,
    watched_keys: &mut HashSet<String>,
) {
    if watched_keys.is_empty() {
        return;
    }
    let mut reg = registry.map.lock().await;
    for key in watched_keys.drain() {
        if let Some(subs) = reg.get_mut(&key) {
            subs.retain(|(id, _)| *id != conn_id);
            if subs.is_empty() {
                reg.remove(&key);
            }
        }
    }
    registry.sync_len(&reg);
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn encode_pubsub_msg(msg: PubSubMsg) -> Vec<u8> {
    match msg {
        PubSubMsg::Message { channel, message } => Value::Push(vec![
            Value::BulkString(Some(b"message".to_vec())),
            Value::BulkString(Some(channel.into_bytes())),
            Value::BulkString(Some(message.into_bytes())),
        ])
        .serialize(),
        PubSubMsg::PMessage {
            pattern,
            channel,
            message,
        } => Value::Push(vec![
            Value::BulkString(Some(b"pmessage".to_vec())),
            Value::BulkString(Some(pattern.into_bytes())),
            Value::BulkString(Some(channel.into_bytes())),
            Value::BulkString(Some(message.into_bytes())),
        ])
        .serialize(),
    }
}

fn resp_subscribe_ack(kind: &str, channel: &str, count: usize) -> Vec<u8> {
    Value::Array(Some(vec![
        Value::BulkString(Some(kind.as_bytes().to_vec())),
        Value::BulkString(Some(channel.as_bytes().to_vec())),
        Value::Integer(count as i64),
    ]))
    .serialize()
}

fn glob_match(pattern: &str, s: &str) -> bool {
    glob_helper(pattern.as_bytes(), s.as_bytes())
}

fn glob_helper(pat: &[u8], s: &[u8]) -> bool {
    match (pat.first(), s.first()) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(b'*'), _) => {
            glob_helper(&pat[1..], s) || (!s.is_empty() && glob_helper(pat, &s[1..]))
        }
        (Some(b'?'), Some(_)) => glob_helper(&pat[1..], &s[1..]),
        (Some(b'?'), None) | (Some(_), None) => false,
        (Some(p), Some(c)) if p == c => glob_helper(&pat[1..], &s[1..]),
        _ => false,
    }
}

/// Encodes a list of string parts as a RESP3 Push frame for WebSocket fan-out.
/// Uses `>` prefix so clients can distinguish server-initiated pushes from command responses.
fn resp_push(parts: &[&str]) -> String {
    let mut s = format!(">{}\r\n", parts.len());
    for part in parts {
        s.push_str(&format!("${}\r\n{}\r\n", part.len(), part));
    }
    s
}

/// Returns the RESP-encoded mutation to broadcast to WebSocket peers, or `None`
/// if the command mutated nothing (read-only or conditional-and-failed).
fn broadcast_for(cmd: &Command, response: &Value) -> Option<String> {
    match cmd {
        Command::Set(k, v, opts) => {
            // Without GET: nil response means NX/XX condition failed — don't broadcast.
            // With GET: nil means key didn't exist before, but SET still happened.
            let set_happened = opts.get || !matches!(response, Value::BulkString(None));
            if !set_happened {
                return None;
            }
            match &opts.expiry {
                None => Some(resp_push(&["SET", k, v])),
                Some(SetExpiry::Ex(s)) => {
                    let px = s.saturating_mul(1000).to_string();
                    Some(resp_push(&["SET", k, v, "PX", &px]))
                }
                Some(SetExpiry::Px(ms)) => {
                    let ms_s = ms.to_string();
                    Some(resp_push(&["SET", k, v, "PX", &ms_s]))
                }
                Some(SetExpiry::Exat(ts)) => {
                    let pxat = ts.saturating_mul(1000).to_string();
                    Some(resp_push(&["SET", k, v, "PXAT", &pxat]))
                }
                Some(SetExpiry::Pxat(ts)) => {
                    let ts_s = ts.to_string();
                    Some(resp_push(&["SET", k, v, "PXAT", &ts_s]))
                }
                Some(SetExpiry::KeepTtl) => Some(resp_push(&["SET", k, v, "KEEPTTL"])),
            }
        }
        Command::Del(keys) | Command::Unlink(keys) => {
            let mut parts: Vec<&str> = vec!["DEL"];
            let key_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
            parts.extend_from_slice(&key_refs);
            Some(resp_push(&parts))
        }
        Command::MSet(pairs) => {
            let mut parts: Vec<&str> = vec!["MSET"];
            let flat: Vec<String> = pairs
                .iter()
                .flat_map(|(k, v)| [k.clone(), v.clone()])
                .collect();
            let flat_refs: Vec<&str> = flat.iter().map(|s| s.as_str()).collect();
            parts.extend_from_slice(&flat_refs);
            Some(resp_push(&parts))
        }
        Command::SetNx(k, v) => match response {
            Value::Integer(1) => Some(resp_push(&["SET", k, v])),
            _ => None,
        },
        Command::SetEx(k, secs, v) => {
            let px = secs.saturating_mul(1000).to_string();
            Some(resp_push(&["SET", k, v, "PX", &px]))
        }
        Command::PSetEx(k, ms, v) => {
            let ms_s = ms.to_string();
            Some(resp_push(&["SET", k, v, "PX", &ms_s]))
        }
        Command::Append(k, v) => match response {
            Value::Integer(_) => Some(resp_push(&["APPEND", k, v])),
            _ => None,
        },
        Command::GetSet(k, v) => Some(resp_push(&["SET", k, v])),
        Command::Incr(k) | Command::Decr(k) => match response {
            Value::Integer(n) => {
                let s = n.to_string();
                Some(resp_push(&["SET", k, &s]))
            }
            _ => None,
        },
        Command::IncrBy(k, _) | Command::DecrBy(k, _) => match response {
            Value::Integer(n) => {
                let s = n.to_string();
                Some(resp_push(&["SET", k, &s]))
            }
            _ => None,
        },
        Command::Expire(k, secs) => match response {
            Value::Integer(1) => {
                let ms = secs.saturating_mul(1000).to_string();
                Some(resp_push(&["PEXPIRE", k, &ms]))
            }
            _ => None,
        },
        Command::PExpire(k, ms) => match response {
            Value::Integer(1) => {
                let ms_s = ms.to_string();
                Some(resp_push(&["PEXPIRE", k, &ms_s]))
            }
            _ => None,
        },
        Command::ExpireAt(k, ts) => match response {
            Value::Integer(1) => {
                let ts_ms = ts.saturating_mul(1000).to_string();
                Some(resp_push(&["PEXPIREAT", k, &ts_ms]))
            }
            _ => None,
        },
        Command::PExpireAt(k, ts) => match response {
            Value::Integer(1) => {
                let ts_s = ts.to_string();
                Some(resp_push(&["PEXPIREAT", k, &ts_s]))
            }
            _ => None,
        },
        Command::Persist(k) => match response {
            Value::Integer(1) => Some(resp_push(&["PERSIST", k])),
            _ => None,
        },
        Command::FlushDb => Some(resp_push(&["FLUSHDB"])),
        Command::Rename(src, dst) => match response {
            Value::Error(_) => None,
            _ => Some(resp_push(&["RENAME", src, dst])),
        },

        // ── Hash ─────────────────────────────────────────────────────────────
        Command::HSet(k, pairs) => {
            let mut parts: Vec<String> = vec!["HSET".into(), k.clone()];
            for (f, v) in pairs {
                parts.push(f.clone());
                parts.push(v.clone());
            }
            let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
            Some(resp_push(&refs))
        }
        Command::HDel(k, fields) => match response {
            Value::Integer(n) if *n > 0 => {
                let mut parts: Vec<&str> = vec!["HDEL", k];
                let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
                parts.extend_from_slice(&field_refs);
                Some(resp_push(&parts))
            }
            _ => None,
        },
        Command::HIncrBy(k, f, _) => match response {
            Value::Integer(n) => {
                let s = n.to_string();
                Some(resp_push(&["HSET", k, f, &s]))
            }
            _ => None,
        },
        Command::HIncrByFloat(k, f, _) => match response {
            Value::BulkString(Some(data)) => {
                let s = String::from_utf8_lossy(data);
                Some(resp_push(&["HSET", k, f, &s]))
            }
            _ => None,
        },
        Command::HSetNx(k, f, v) => match response {
            Value::Integer(1) => Some(resp_push(&["HSET", k, f, v])),
            _ => None,
        },

        // ── List ─────────────────────────────────────────────────────────────
        Command::LPush(k, vals) | Command::RPush(k, vals) => {
            let cmd_name = if matches!(cmd, Command::LPush(_, _)) {
                "LPUSH"
            } else {
                "RPUSH"
            };
            let mut parts: Vec<&str> = vec![cmd_name, k];
            let val_refs: Vec<&str> = vals.iter().map(|s| s.as_str()).collect();
            parts.extend_from_slice(&val_refs);
            Some(resp_push(&parts))
        }
        Command::LPushX(k, vals) | Command::RPushX(k, vals) => match response {
            Value::Integer(n) if *n > 0 => {
                let cmd_name = if matches!(cmd, Command::LPushX(_, _)) {
                    "LPUSH"
                } else {
                    "RPUSH"
                };
                let mut parts: Vec<&str> = vec![cmd_name, k];
                let val_refs: Vec<&str> = vals.iter().map(|s| s.as_str()).collect();
                parts.extend_from_slice(&val_refs);
                Some(resp_push(&parts))
            }
            _ => None,
        },
        Command::LPop(k, count) => match response {
            Value::BulkString(None) => None,
            Value::Array(Some(items)) if items.is_empty() => None,
            _ => {
                let n = count.map(|c| c.to_string());
                match &n {
                    Some(ns) => Some(resp_push(&["LPOP", k, ns])),
                    None => Some(resp_push(&["LPOP", k])),
                }
            }
        },
        Command::RPop(k, count) => match response {
            Value::BulkString(None) => None,
            Value::Array(Some(items)) if items.is_empty() => None,
            _ => {
                let n = count.map(|c| c.to_string());
                match &n {
                    Some(ns) => Some(resp_push(&["RPOP", k, ns])),
                    None => Some(resp_push(&["RPOP", k])),
                }
            }
        },
        Command::LSet(k, idx, v) => match response {
            Value::SimpleString(_) => {
                let idx_s = idx.to_string();
                Some(resp_push(&["LSET", k, &idx_s, v]))
            }
            _ => None,
        },
        Command::LRem(k, count, elem) => match response {
            Value::Integer(n) if *n > 0 => {
                let count_s = count.to_string();
                Some(resp_push(&["LREM", k, &count_s, elem]))
            }
            _ => None,
        },
        Command::LTrim(k, start, stop) => {
            let start_s = start.to_string();
            let stop_s = stop.to_string();
            Some(resp_push(&["LTRIM", k, &start_s, &stop_s]))
        }

        // ── Set ───────────────────────────────────────────────────────────────
        Command::SAdd(k, members) => match response {
            Value::Integer(n) if *n > 0 => {
                let mut parts: Vec<&str> = vec!["SADD", k];
                let m_refs: Vec<&str> = members.iter().map(|s| s.as_str()).collect();
                parts.extend_from_slice(&m_refs);
                Some(resp_push(&parts))
            }
            _ => None,
        },
        Command::SRem(k, members) => match response {
            Value::Integer(n) if *n > 0 => {
                let mut parts: Vec<&str> = vec!["SREM", k];
                let m_refs: Vec<&str> = members.iter().map(|s| s.as_str()).collect();
                parts.extend_from_slice(&m_refs);
                Some(resp_push(&parts))
            }
            _ => None,
        },
        Command::SPop(k, count) => {
            let popped: Vec<String> = match response {
                Value::BulkString(Some(data)) => {
                    vec![String::from_utf8_lossy(data).into_owned()]
                }
                Value::Array(Some(items)) => items
                    .iter()
                    .filter_map(|v| {
                        if let Value::BulkString(Some(d)) = v {
                            Some(String::from_utf8_lossy(d).into_owned())
                        } else {
                            None
                        }
                    })
                    .collect(),
                _ => vec![],
            };
            if popped.is_empty() {
                let _ = count;
                None
            } else {
                let mut parts: Vec<&str> = vec!["SREM", k];
                let m_refs: Vec<&str> = popped.iter().map(|s| s.as_str()).collect();
                parts.extend_from_slice(&m_refs);
                Some(resp_push(&parts))
            }
        }
        Command::SMove(src, dst, member) => match response {
            Value::Integer(1) => Some(resp_push(&["SMOVE", src, dst, member])),
            _ => None,
        },
        Command::SInterStore(dst, keys) => {
            let mut parts: Vec<&str> = vec!["SINTERSTORE", dst];
            let k_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
            parts.extend_from_slice(&k_refs);
            Some(resp_push(&parts))
        }
        Command::SUnionStore(dst, keys) => {
            let mut parts: Vec<&str> = vec!["SUNIONSTORE", dst];
            let k_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
            parts.extend_from_slice(&k_refs);
            Some(resp_push(&parts))
        }
        Command::SDiffStore(dst, keys) => {
            let mut parts: Vec<&str> = vec!["SDIFFSTORE", dst];
            let k_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
            parts.extend_from_slice(&k_refs);
            Some(resp_push(&parts))
        }

        // ── Sorted Set ────────────────────────────────────────────────────────
        Command::ZAdd(k, opts, pairs) => {
            let mut parts: Vec<String> = vec!["ZADD".into(), k.clone()];
            if let Some(cond) = &opts.condition {
                parts.push(match cond {
                    ZAddCondition::Nx => "NX".into(),
                    ZAddCondition::Xx => "XX".into(),
                });
            }
            if opts.ch {
                parts.push("CH".into());
            }
            if opts.incr {
                parts.push("INCR".into());
            }
            for (score, member) in pairs {
                parts.push(format_f64_score(*score));
                parts.push(member.clone());
            }
            let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
            Some(resp_push(&refs))
        }
        Command::ZRem(k, members) => match response {
            Value::Integer(n) if *n > 0 => {
                let mut parts: Vec<&str> = vec!["ZREM", k];
                let m_refs: Vec<&str> = members.iter().map(|s| s.as_str()).collect();
                parts.extend_from_slice(&m_refs);
                Some(resp_push(&parts))
            }
            _ => None,
        },
        Command::ZIncrBy(k, delta, member) => {
            let delta_s = format_f64_score(*delta);
            Some(resp_push(&["ZINCRBY", k, &delta_s, member]))
        }

        // ── JSON ─────────────────────────────────────────────────────────────
        // Replayable as-is on replicas, AOF, and browser stores. Only
        // successful writes replicate (errors reply -ERR, not +OK).
        Command::JSet(k, path, value) => match response {
            Value::SimpleString(_) => Some(resp_push(&["JSET", k, path, value])),
            _ => None,
        },
        Command::JMerge(k, patch) => match response {
            Value::SimpleString(_) => Some(resp_push(&["JMERGE", k, patch])),
            _ => None,
        },

        // ── Rate limiting ────────────────────────────────────────────────────
        // RLSET replicates so limiter *config* survives AOF replay / reaches
        // replicas. RLCHECK is deliberately not replicated: attempt state is
        // transient and high-frequency — streaming every check would flood the
        // AOF and the sync fan-out for state that expires within one window.
        Command::RlSet(k, limit, window_secs) => {
            let limit_s = limit.to_string();
            let window_s = window_secs.to_string();
            Some(resp_push(&["RLSET", k, &limit_s, &window_s]))
        }

        // Pub/Sub and transactions carry no store state — no broadcast needed.
        _ => None,
    }
}

fn format_f64_score(s: f64) -> String {
    if s == f64::INFINITY {
        "inf".into()
    } else if s == f64::NEG_INFINITY {
        "-inf".into()
    } else if s.fract() == 0.0 && s.abs() < 1e15 {
        format!("{}", s as i64)
    } else {
        format!("{}", s)
    }
}

/// Handles an AUTH attempt. Returns `(disconnect, resp_bytes)`.
///
/// `disconnect` is true when the failure count hits MAX_AUTH_FAILURES.
fn process_auth(
    provided: &str,
    expected: &Arc<Option<String>>,
    is_authenticated: &mut bool,
    failures: &mut u32,
) -> (bool, Vec<u8>) {
    match expected.as_ref() {
        // Constant-time compare so a network attacker can't recover the password
        // byte-by-byte from response-timing differences.
        Some(pwd) if ct_eq_bytes(provided.as_bytes(), pwd.as_bytes()) => {
            *is_authenticated = true;
            *failures = 0;
            (false, b"+OK\r\n".to_vec())
        }
        Some(_) => {
            *failures += 1;
            if *failures >= MAX_AUTH_FAILURES {
                (true, b"-ERR too many authentication failures\r\n".to_vec())
            } else {
                (false, b"-ERR invalid password\r\n".to_vec())
            }
        }
        None => (
            false,
            b"-ERR Client sent AUTH, but no password is set\r\n".to_vec(),
        ),
    }
}

// ── main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // All runtime configuration is via RECACHED_* env vars; the only flags are
    // --version/-V (required by e.g. the Homebrew formula's install test).
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("recached-server {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // ── bind address ──────────────────────────────────────────────────────
    // Host/interface all listeners bind to. Defaults to 0.0.0.0 (all
    // interfaces) for backwards compatibility; set RECACHED_BIND=127.0.0.1 to
    // restrict to localhost, which — together with RECACHED_PASSWORD — is
    // strongly recommended unless the server is deliberately public.
    let bind_host = std::env::var("RECACHED_BIND").unwrap_or_else(|_| "0.0.0.0".to_string());
    if bind_host == "0.0.0.0" {
        warn!(
            "Binding all interfaces (0.0.0.0). Set RECACHED_BIND=127.0.0.1 and RECACHED_PASSWORD before exposing this host."
        );
    } else {
        info!("Binding interface {}", bind_host);
    }

    // ── Prometheus metrics ────────────────────────────────────────────────
    let metrics_port: u16 = std::env::var("RECACHED_METRICS_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9091);
    let metrics_addr: std::net::SocketAddr =
        format!("{}:{}", bind_host, metrics_port).parse().unwrap();
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .with_http_listener(metrics_addr)
        .install()
        .expect("failed to install Prometheus metrics exporter");
    info!("Prometheus metrics at http://{}/metrics", metrics_addr);

    // ── auth ──────────────────────────────────────────────────────────────
    let password = std::env::var("RECACHED_PASSWORD").ok();
    let global_password = Arc::new(password);

    if global_password.is_some() {
        info!("Authentication ENABLED. Clients must send 'AUTH <password>'.");
    } else {
        warn!("Authentication DISABLED. Set RECACHED_PASSWORD to enable.");
    }

    // ── sync scoping ──────────────────────────────────────────────────────
    let sync_secret: Arc<Option<String>> = Arc::new(
        std::env::var("RECACHED_SYNC_SECRET")
            .ok()
            .filter(|s| !s.is_empty()),
    );
    if sync_secret.is_some() {
        info!(
            "Sync scoping ENABLED (strict): WebSocket clients receive no pushes and no key access until they present 'SYNC TOKEN <token>'."
        );
    } else {
        warn!(
            "Sync scoping DISABLED: every WebSocket client receives every mutation. Set RECACHED_SYNC_SECRET before exposing port 6380 to untrusted clients."
        );
    }

    // ── IP allowlist ──────────────────────────────────────────────────────
    let allowed_ips: Option<Arc<Vec<IpAddr>>> = std::env::var("RECACHED_ALLOW_IPS").ok().map(|s| {
        let ips: Vec<IpAddr> = s
            .split(',')
            .filter_map(|raw| {
                let trimmed = raw.trim();
                match IpAddr::from_str(trimmed) {
                    Ok(ip) => Some(ip),
                    Err(_) => {
                        warn!("RECACHED_ALLOW_IPS: ignoring invalid entry '{}'", trimmed);
                        None
                    }
                }
            })
            .collect();
        Arc::new(ips)
    });

    if let Some(ips) = &allowed_ips {
        info!("IP allowlist ENABLED: {:?}", ips);
    } else {
        warn!("IP allowlist DISABLED. Accepting all connections.");
    }

    // ── store ─────────────────────────────────────────────────────────────
    let max_keys = std::env::var("RECACHED_MAX_KEYS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok());

    let max_memory_bytes = std::env::var("RECACHED_MAX_MEMORY")
        .ok()
        .and_then(|v| parse_memory_bytes(&v));

    let eviction_policy = match std::env::var("RECACHED_EVICTION")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "allkeys-lru" | "lru" => EvictionPolicy::AllKeysLru,
        "allkeys-random" | "random" => EvictionPolicy::AllKeysRandom,
        "volatile-lru" => EvictionPolicy::VolatileLru,
        "volatile-ttl" | "ttl" => EvictionPolicy::VolatileTtl,
        _ => EvictionPolicy::NoEviction,
    };

    if max_keys.is_some() || max_memory_bytes.is_some() {
        info!(
            "Key limit: {:?}, memory limit: {:?} bytes, eviction: {:?}",
            max_keys, max_memory_bytes, eviction_policy
        );
    }

    let store = Arc::new(KeyValueStore::with_config(
        max_keys,
        max_memory_bytes,
        eviction_policy,
    ));

    // ── snapshot persistence ──────────────────────────────────────────────
    let save_path = PathBuf::from(
        std::env::var("RECACHED_SAVE_PATH").unwrap_or_else(|_| "recached.rdb".to_string()),
    );

    // RECACHED_SAVE takes priority: "900:1,300:10,60:10000" (secs:changes pairs).
    // Falls back to RECACHED_SAVE_INTERVAL (single-condition, 1 change required).
    let save_conditions: Vec<SaveCondition> = if let Ok(s) = std::env::var("RECACHED_SAVE") {
        parse_save_conditions(&s)
    } else {
        let interval: u64 = std::env::var("RECACHED_SAVE_INTERVAL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(900);
        if interval > 0 {
            vec![SaveCondition {
                secs: interval,
                changes: 1,
            }]
        } else {
            vec![]
        }
    };

    load_snapshot(&store, &save_path).await;

    let snap_cfg = Arc::new(SnapshotConfig {
        path: save_path,
        last_save: AtomicI64::new(now_unix_secs()),
    });

    // ── AOF ───────────────────────────────────────────────────────────────
    let aof_path = std::env::var("RECACHED_AOF_PATH").ok().map(PathBuf::from);
    let aof_sync = match std::env::var("RECACHED_AOF_SYNC")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "always" => AofSync::Always,
        "no" => AofSync::No,
        _ => AofSync::EverySec,
    };

    let aof: Option<Arc<AofWriter>> = if let Some(path) = aof_path {
        match AofWriter::open(path.clone(), aof_sync).await {
            Ok(w) => {
                replay_aof(&store, &path).await;
                let writer = Arc::new(w);
                if aof_sync == AofSync::EverySec {
                    let w2 = Arc::clone(&writer);
                    tokio::spawn(async move {
                        let mut interval =
                            tokio::time::interval(tokio::time::Duration::from_secs(1));
                        loop {
                            interval.tick().await;
                            w2.flush().await;
                        }
                    });
                }
                info!(
                    "AOF enabled: {:?} (sync={})",
                    path,
                    match aof_sync {
                        AofSync::Always => "always",
                        AofSync::EverySec => "everysec",
                        AofSync::No => "no",
                    }
                );
                Some(writer)
            }
            Err(e) => {
                warn!("AOF open failed: {} — running without AOF", e);
                None
            }
        }
    } else {
        None
    };

    // ── Replication ───────────────────────────────────────────────────────
    let replicaof = std::env::var("RECACHED_REPLICAOF").ok();
    let repl_port: u16 = std::env::var("RECACHED_REPL_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6381);
    let repl_password: Option<String> = std::env::var("RECACHED_REPL_PASSWORD").ok();
    let repl_channel_capacity: usize = std::env::var("RECACHED_REPL_BUFFER")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(DEFAULT_REPL_CHANNEL_CAPACITY);
    let failover_timeout_secs: Option<u64> = std::env::var("RECACHED_FAILOVER_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0);

    if repl_password.is_some() {
        info!("Replication auth ENABLED (RECACHED_REPL_PASSWORD is set).");
    } else {
        warn!(
            "Replication auth DISABLED. Set RECACHED_REPL_PASSWORD to secure the replication port."
        );
    }

    let is_replica_start = replicaof.is_some();
    let replicas: ReplRegistry = ReplHub::new();

    // ── server state ──────────────────────────────────────────────────────
    let state = Arc::new(ServerState {
        snap: Arc::clone(&snap_cfg),
        aof,
        replicas: Arc::clone(&replicas),
        is_replica: std::sync::atomic::AtomicBool::new(is_replica_start),
        dedup: std::sync::Mutex::new(HashMap::new()),
    });

    // ── autosave ──────────────────────────────────────────────────────────
    if !save_conditions.is_empty() {
        let store_snap = Arc::clone(&store);
        let state_snap = Arc::clone(&state);
        let conditions = save_conditions;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(1));
            ticker.tick().await; // skip immediate first tick
            loop {
                ticker.tick().await;
                let now = now_unix_secs();
                let last = state_snap.snap.last_save.load(Ordering::Relaxed);
                let elapsed = now.saturating_sub(last).max(0) as u64;
                let dirty = store_snap.dirty_count();
                if dirty > 0
                    && conditions
                        .iter()
                        .any(|c| elapsed >= c.secs && dirty >= c.changes)
                {
                    state_snap.save(&store_snap).await;
                }
            }
        });
        info!("Autosave active → {:?}", snap_cfg.path);
    } else {
        info!(
            "Autosave disabled (RECACHED_SAVE=0 or RECACHED_SAVE_INTERVAL=0). Use SAVE or BGSAVE manually."
        );
    }

    // ── broadcast channel (mutation sync) ────────────────────────────────
    // Carries (sender_conn_id, resp_encoded_mutation). WS receivers skip their
    // own messages. Created before replication so a replica can push the writes
    // it receives from the primary to its own local WebSocket clients.
    let (tx, _rx) = broadcast::channel::<SyncMsg>(BROADCAST_CHANNEL_CAPACITY);

    // ── start replication ─────────────────────────────────────────────────
    // The replication server runs on every node — including replicas — so a
    // replica can in turn serve sub-replicas (multi-tier replication).
    {
        let store_r = Arc::clone(&store);
        let snap_r = Arc::clone(&snap_cfg);
        let reg_r = Arc::clone(&replicas);
        let pwd_r = repl_password.clone().map(Arc::new);
        let cap_r = repl_channel_capacity;
        let host_r = bind_host.clone();
        tokio::spawn(async move {
            run_repl_server(host_r, repl_port, store_r, snap_r, reg_r, pwd_r, cap_r).await;
        });
    }
    if is_replica_start && let Some(primary_addr) = replicaof {
        let store_r = Arc::clone(&store);
        let state_r = Arc::clone(&state);
        let pwd_r = repl_password.clone();
        let fo_r = failover_timeout_secs;
        let tx_r = tx.clone();
        tokio::spawn(async move {
            run_repl_client(primary_addr, store_r, state_r, pwd_r, fo_r, tx_r).await;
        });
        if let Some(t) = failover_timeout_secs {
            info!(
                "Running as replica — auto-failover enabled (promotes after {}s of primary being unreachable)",
                t
            );
        } else {
            info!(
                "Running as replica — write commands will be rejected (auto-failover disabled; set RECACHED_FAILOVER_TIMEOUT to enable)"
            );
        }
    }

    // ── background eviction ───────────────────────────────────────────────
    {
        let store_sweep = Arc::clone(&store);
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(EVICTION_INTERVAL_SECS));
            loop {
                interval.tick().await;
                store_sweep.sweep_expired();
                store_sweep.try_evict_for_memory();
            }
        });
    }

    // ── pub/sub hub ───────────────────────────────────────────────────────
    let pubsub: SharedPubSub = Arc::new(tokio::sync::Mutex::new(PubSubHub::new()));

    // ── watch registry ────────────────────────────────────────────────────
    let watch_registry: WatchRegistry = WatchHub::new();

    // ── connection limiter ────────────────────────────────────────────────
    let max_connections = std::env::var("RECACHED_MAX_CONNECTIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_CONNECTIONS);
    info!("Max connections: {}", max_connections);
    let semaphore = Arc::new(Semaphore::new(max_connections));

    // ── TLS ───────────────────────────────────────────────────────────────
    let tls_acceptor: Option<TlsAcceptor> = load_tls_acceptor();
    if tls_acceptor.is_some() {
        info!(
            "TLS ENABLED (cert={}, key={})",
            std::env::var("RECACHED_TLS_CERT").unwrap_or_default(),
            std::env::var("RECACHED_TLS_KEY").unwrap_or_default()
        );
    } else {
        warn!("TLS DISABLED. Set RECACHED_TLS_CERT and RECACHED_TLS_KEY to enable.");
    }
    let tls_acceptor = Arc::new(tls_acceptor);

    // ── listeners ─────────────────────────────────────────────────────────
    let n_accept = num_cpus::get();
    let tcp_listeners = make_tcp_listeners(&format!("{}:6379", bind_host), n_accept)?;
    info!(
        "TCP server listening on {}:6379 ({} accept loop(s))",
        bind_host, n_accept
    );

    let ws_listener = TcpListener::bind(format!("{}:6380", bind_host)).await?;
    info!("WebSocket server listening on {}:6380", bind_host);

    // Spawn one accept loop per CPU core, each with its own SO_REUSEPORT socket.
    // The OS load-balances incoming connections across all loops.
    for tcp_listener in tcp_listeners {
        let store_tcp = Arc::clone(&store);
        let tx_tcp = tx.clone();
        let pass_tcp = Arc::clone(&global_password);
        let allowed_tcp = allowed_ips.clone();
        let sem_tcp = Arc::clone(&semaphore);
        let pubsub_tcp = Arc::clone(&pubsub);
        let tls_tcp = Arc::clone(&tls_acceptor);
        let watch_tcp = Arc::clone(&watch_registry);
        let snap_tcp = Arc::clone(&state);

        tokio::spawn(async move {
            loop {
                match tcp_listener.accept().await {
                    Ok((socket, addr)) => {
                        let _ = socket.set_nodelay(true);
                        if let Some(allowed) = &allowed_tcp
                            && !allowed.contains(&addr.ip())
                        {
                            debug!("TCP: rejected IP {}", addr.ip());
                            continue;
                        }
                        let permit = match Arc::clone(&sem_tcp).try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                warn!("TCP: connection limit reached, dropping {}", addr);
                                continue;
                            }
                        };
                        let s = Arc::clone(&store_tcp);
                        let t = tx_tcp.clone();
                        let p = Arc::clone(&pass_tcp);
                        let ps = Arc::clone(&pubsub_tcp);
                        let wr = Arc::clone(&watch_tcp);
                        let tls = Arc::clone(&tls_tcp);
                        let sc = Arc::clone(&snap_tcp);
                        tokio::spawn(async move {
                            let _permit = permit;
                            if let Some(acc) = tls.as_ref() {
                                match acc.accept(socket).await {
                                    Ok(tls_stream) => {
                                        handle_tcp(tls_stream, s, t, p, ps, wr, sc).await
                                    }
                                    Err(e) => {
                                        warn!("TCP TLS handshake failed from {}: {}", addr, e)
                                    }
                                }
                            } else {
                                handle_tcp(socket, s, t, p, ps, wr, sc).await;
                            }
                        });
                    }
                    Err(e) => warn!("TCP accept error: {}", e),
                }
            }
        });
    }

    // ── graceful shutdown via oneshot channel ────────────────────────────
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to register SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = sigterm.recv() => {},
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        let _ = shutdown_tx.send(());
    });

    loop {
        tokio::select! {
            biased;

            res = ws_listener.accept() => {
                match res {
                    Ok((socket, addr)) => {
                        let _ = socket.set_nodelay(true);
                        if let Some(allowed) = &allowed_ips
                            && !allowed.contains(&addr.ip())
                        {
                            debug!("WS: rejected IP {}", addr.ip());
                            continue;
                        }
                        let permit = match Arc::clone(&semaphore).try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                warn!("WS: connection limit reached, dropping {}", addr);
                                continue;
                            }
                        };
                        let s = Arc::clone(&store);
                        let t = tx.clone();
                        let p = Arc::clone(&global_password);
                        let ps = Arc::clone(&pubsub);
                        let wr = Arc::clone(&watch_registry);
                        let tls = Arc::clone(&tls_acceptor);
                        let sc = Arc::clone(&state);
                        let ss = Arc::clone(&sync_secret);
                        let id = next_conn_id();
                        tokio::spawn(async move {
                            let _permit = permit;
                            if let Some(acc) = tls.as_ref() {
                                match acc.accept(socket).await {
                                    Ok(tls_stream) => handle_ws(tls_stream, s, t, p, id, ps, wr, sc, ss).await,
                                    Err(e) => warn!("WS TLS handshake failed from {}: {}", addr, e),
                                }
                            } else {
                                handle_ws(socket, s, t, p, id, ps, wr, sc, ss).await;
                            }
                        });
                    }
                    Err(e) => warn!("WS accept error: {}", e),
                }
            }

            _ = &mut shutdown_rx => {
                info!("Shutdown signal received, saving final snapshot...");
                state.save(&store).await;
                info!("Done. Goodbye.");
                break;
            }
        }
    }

    Ok(())
}

// ── TCP handler ───────────────────────────────────────────────────────────────

async fn handle_tcp<S>(
    socket: S,
    store: Arc<KeyValueStore>,
    tx: broadcast::Sender<SyncMsg>,
    password: Arc<Option<String>>,
    pubsub: SharedPubSub,
    watch_registry: WatchRegistry,
    state: Arc<ServerState>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let _guard = ConnectionGuard::tcp();
    let (mut reader, raw_writer) = tokio::io::split(socket);
    let mut writer = tokio::io::BufWriter::with_capacity(32 * 1024, raw_writer);
    let mut buf = Vec::<u8>::new();
    let mut read_pos: usize = 0;
    let mut read_buf = [0u8; TCP_READ_BUFFER_BYTES];
    // Reused for every response on this connection — avoids a Vec allocation
    // per command (significant under pipelining).
    let mut resp_buf = Vec::<u8>::with_capacity(4 * 1024);
    let mut is_authenticated = password.is_none();
    let mut auth_failures: u32 = 0;
    let mut multi_queue: Option<Vec<Command>> = None;
    let mut subscribed_channels: HashSet<String> = HashSet::new();
    let mut subscribed_patterns: HashSet<String> = HashSet::new();
    let (ps_tx, mut ps_rx) = mpsc::unbounded_channel::<PubSubMsg>();
    // WATCH state for optimistic-lock transactions over TCP. Unlike the WS
    // handler, TCP clients are not sent keychange pushes — WATCH is pure CAS.
    let mut watched_keys: HashSet<String> = HashSet::new();
    let mut watch_dirty = false;
    let (watch_tx, mut watch_rx) = mpsc::unbounded_channel::<WatchNotif>();
    let conn_id = next_conn_id();

    'outer: loop {
        let is_subscribed = !subscribed_channels.is_empty() || !subscribed_patterns.is_empty();

        tokio::select! {
            result = reader.read(&mut read_buf) => {
                match result {
                    Ok(0) => break,
                    Ok(n) => {
                        if (buf.len() - read_pos) + n > MAX_TCP_READ_BUFFER_BYTES {
                            warn!("TCP connection exceeded max buffer size, closing");
                            break 'outer;
                        }
                        buf.extend_from_slice(&read_buf[..n]);
                        'parse: loop {
                            match Value::parse(&buf[read_pos..]) {
                                Ok((value, consumed)) => {
                                    read_pos += consumed;
                                    let cmd = match Command::from_value(value) {
                                        Ok(c) => c,
                                        Err(e) => {
                                            let r = Value::Error(e).serialize();
                                            if writer.write_all(&r).await.is_err() { break 'outer; }
                                            continue 'parse;
                                        }
                                    };

                                    // AUTH is always processed immediately
                                    if let Command::Auth(ref pwd) = cmd {
                                        let (disconnect, resp) = process_auth(
                                            pwd, &password, &mut is_authenticated, &mut auth_failures,
                                        );
                                        if writer.write_all(&resp).await.is_err() { break 'outer; }
                                        if disconnect {
                                            let _ = writer.flush().await;
                                            break 'outer;
                                        }
                                        continue 'parse;
                                    }

                                    if !is_authenticated {
                                        if writer.write_all(b"-NOAUTH Authentication required.\r\n").await.is_err() {
                                            break 'outer;
                                        }
                                        continue 'parse;
                                    }

                                    // ── Transactions ──────────────────────────────
                                    match &cmd {
                                        Command::Multi => {
                                            let resp = if multi_queue.is_some() {
                                                b"-ERR MULTI calls can not be nested\r\n".to_vec()
                                            } else {
                                                multi_queue = Some(Vec::new());
                                                b"+OK\r\n".to_vec()
                                            };
                                            if writer.write_all(&resp).await.is_err() { break 'outer; }
                                            continue 'parse;
                                        }
                                        Command::Discard => {
                                            let resp = if multi_queue.take().is_some() {
                                                // DISCARD also flushes WATCH state.
                                                unregister_all_watches(&watch_registry, conn_id, &mut watched_keys).await;
                                                while watch_rx.try_recv().is_ok() {}
                                                watch_dirty = false;
                                                b"+OK\r\n".to_vec()
                                            } else {
                                                b"-ERR DISCARD without MULTI\r\n".to_vec()
                                            };
                                            if writer.write_all(&resp).await.is_err() { break 'outer; }
                                            continue 'parse;
                                        }
                                        Command::Exec => {
                                            match multi_queue.take() {
                                                None => {
                                                    if writer.write_all(b"-ERR EXEC without MULTI\r\n").await.is_err() { break 'outer; }
                                                }
                                                Some(queue) => {
                                                    // Drain pending notifications so the CAS check isn't racy.
                                                    while watch_rx.try_recv().is_ok() {
                                                        watch_dirty = true;
                                                    }
                                                    if watch_dirty {
                                                        // A watched key changed since WATCH — abort with nil array.
                                                        unregister_all_watches(&watch_registry, conn_id, &mut watched_keys).await;
                                                        while watch_rx.try_recv().is_ok() {}
                                                        watch_dirty = false;
                                                        if writer.write_all(&Value::Array(None).serialize()).await.is_err() { break 'outer; }
                                                    } else {
                                                        let mut results = Vec::with_capacity(queue.len());
                                                        let armed = write_effects_armed(&tx, &state, &watch_registry);
                                                        for qcmd in queue {
                                                            let resp = if armed && is_write_command(&qcmd) {
                                                                let resp = execute_and_record(&store, qcmd.clone());
                                                                apply_write_effects(&qcmd, &resp, &tx, 0, &state, &watch_registry, &store).await;
                                                                resp
                                                            } else {
                                                                execute_and_record(&store, qcmd)
                                                            };
                                                            results.push(resp);
                                                        }
                                                        unregister_all_watches(&watch_registry, conn_id, &mut watched_keys).await;
                                                        while watch_rx.try_recv().is_ok() {}
                                                        watch_dirty = false;
                                                        let out = Value::Array(Some(results)).serialize();
                                                        if writer.write_all(&out).await.is_err() { break 'outer; }
                                                    }
                                                }
                                            }
                                            continue 'parse;
                                        }
                                        _ => {}
                                    }

                                    // If inside MULTI, queue the command
                                    if let Some(ref mut queue) = multi_queue {
                                        // Pub/sub and WATCH commands cannot be queued
                                        match &cmd {
                                            Command::Subscribe(_) | Command::Unsubscribe(_)
                                            | Command::PSubscribe(_) | Command::PUnsubscribe(_)
                                            | Command::Publish(_, _)
                                            | Command::Watch(_) | Command::Unwatch(_)
                                            | Command::QSub(_) | Command::QUnsub(_) => {
                                                let err = b"-ERR Command not allowed inside a transaction\r\n";
                                                if writer.write_all(err).await.is_err() { break 'outer; }
                                            }
                                            _ => {
                                                if queue.len() >= MAX_MULTI_QUEUE_LEN {
                                                    let err = b"-ERR transaction queue limit reached\r\n";
                                                    if writer.write_all(err).await.is_err() { break 'outer; }
                                                } else {
                                                    queue.push(cmd);
                                                    if writer.write_all(b"+QUEUED\r\n").await.is_err() { break 'outer; }
                                                }
                                            }
                                        }
                                        continue 'parse;
                                    }

                                    // ── Pub/Sub commands ──────────────────────────
                                    match cmd {
                                        Command::Subscribe(channels) => {
                                            for ch in channels {
                                                subscribed_channels.insert(ch.clone());
                                                pubsub.lock().await.subscribe(conn_id, &ch, ps_tx.clone());
                                                let count = subscribed_channels.len() + subscribed_patterns.len();
                                                let ack = resp_subscribe_ack("subscribe", &ch, count);
                                                if writer.write_all(&ack).await.is_err() { break 'outer; }
                                            }
                                        }
                                        Command::Unsubscribe(channels) => {
                                            let targets: Vec<String> = if channels.is_empty() {
                                                subscribed_channels.drain().collect()
                                            } else {
                                                channels.into_iter().filter(|c| subscribed_channels.remove(c)).collect()
                                            };
                                            for ch in &targets {
                                                pubsub.lock().await.unsubscribe(conn_id, ch);
                                                let count = subscribed_channels.len() + subscribed_patterns.len();
                                                let ack = resp_subscribe_ack("unsubscribe", ch, count);
                                                if writer.write_all(&ack).await.is_err() { break 'outer; }
                                            }
                                            if targets.is_empty() {
                                                let ack = resp_subscribe_ack("unsubscribe", "", 0);
                                                if writer.write_all(&ack).await.is_err() { break 'outer; }
                                            }
                                        }
                                        Command::PSubscribe(patterns) => {
                                            for pat in patterns {
                                                subscribed_patterns.insert(pat.clone());
                                                pubsub.lock().await.psubscribe(conn_id, &pat, ps_tx.clone());
                                                let count = subscribed_channels.len() + subscribed_patterns.len();
                                                let ack = resp_subscribe_ack("psubscribe", &pat, count);
                                                if writer.write_all(&ack).await.is_err() { break 'outer; }
                                            }
                                        }
                                        Command::PUnsubscribe(patterns) => {
                                            let targets: Vec<String> = if patterns.is_empty() {
                                                subscribed_patterns.drain().collect()
                                            } else {
                                                patterns.into_iter().filter(|p| subscribed_patterns.remove(p)).collect()
                                            };
                                            for pat in &targets {
                                                pubsub.lock().await.punsubscribe(conn_id, pat);
                                                let count = subscribed_channels.len() + subscribed_patterns.len();
                                                let ack = resp_subscribe_ack("punsubscribe", pat, count);
                                                if writer.write_all(&ack).await.is_err() { break 'outer; }
                                            }
                                            if targets.is_empty() {
                                                let ack = resp_subscribe_ack("punsubscribe", "", 0);
                                                if writer.write_all(&ack).await.is_err() { break 'outer; }
                                            }
                                        }
                                        Command::Publish(channel, message) => {
                                            let count = pubsub.lock().await.publish(&channel, &message);
                                            let resp = Value::Integer(count).serialize();
                                            if writer.write_all(&resp).await.is_err() { break 'outer; }
                                        }

                                        Command::Watch(keys) => {
                                            let new_count = keys.iter().filter(|k| !watched_keys.contains(*k)).count();
                                            if watched_keys.len() + new_count > MAX_WATCHES_PER_CONN {
                                                if writer.write_all(b"-ERR watch limit per connection reached\r\n").await.is_err() { break 'outer; }
                                            } else {
                                                {
                                                    let mut reg = watch_registry.map.lock().await;
                                                    for key in &keys {
                                                        if watched_keys.insert(key.clone()) {
                                                            reg.entry(key.clone()).or_default().push((conn_id, watch_tx.clone()));
                                                        }
                                                    }
                                                    watch_registry.sync_len(&reg);
                                                }
                                                if writer.write_all(b"+OK\r\n").await.is_err() { break 'outer; }
                                            }
                                        }
                                        Command::Unwatch(keys) => {
                                            let targets: Vec<String> = if keys.is_empty() {
                                                watched_keys.drain().collect()
                                            } else {
                                                keys.into_iter().filter(|k| watched_keys.remove(k)).collect()
                                            };
                                            {
                                                let mut reg = watch_registry.map.lock().await;
                                                for key in &targets {
                                                    if let Some(subs) = reg.get_mut(key) {
                                                        subs.retain(|(id, _)| *id != conn_id);
                                                        if subs.is_empty() { reg.remove(key); }
                                                    }
                                                }
                                                watch_registry.sync_len(&reg);
                                            }
                                            if watched_keys.is_empty() {
                                                while watch_rx.try_recv().is_ok() {}
                                                watch_dirty = false;
                                            }
                                            if writer.write_all(b"+OK\r\n").await.is_err() { break 'outer; }
                                        }

                                        cmd => {
                                            // In subscribe mode only ping is allowed
                                            if is_subscribed && !matches!(cmd, Command::Ping(_)) {
                                                let err = b"-ERR only (P)SUBSCRIBE / (P)UNSUBSCRIBE / PING / QUIT allowed in subscribe mode\r\n";
                                                if writer.write_all(err).await.is_err() { break 'outer; }
                                                continue 'parse;
                                            }
                                            // Replica: reject writes
                                            if state.is_replica() && is_write_command(&cmd) {
                                                let err = b"-READONLY You can't write against a read only replica.\r\n";
                                                if writer.write_all(err).await.is_err() { break 'outer; }
                                                continue 'parse;
                                            }
                                            // Snapshot commands — handled here (async I/O, not in execute())
                                            match &cmd {
                                                Command::Save => {
                                                    state.save(&store).await;
                                                    if writer.write_all(b"+OK\r\n").await.is_err() { break 'outer; }
                                                    continue 'parse;
                                                }
                                                Command::BgSave => {
                                                    let s = Arc::clone(&store);
                                                    let st = Arc::clone(&state);
                                                    tokio::spawn(async move { st.save(&s).await; });
                                                    if writer.write_all(b"+Background saving started\r\n").await.is_err() { break 'outer; }
                                                    continue 'parse;
                                                }
                                                Command::LastSave => {
                                                    let ts = state.snap.last_save.load(Ordering::Relaxed);
                                                    if writer.write_all(&Value::Integer(ts).serialize()).await.is_err() { break 'outer; }
                                                    continue 'parse;
                                                }
                                                Command::ReplicaOfNoOne => {
                                                    state.promote_to_primary();
                                                    if writer.write_all(b"+OK\r\n").await.is_err() { break 'outer; }
                                                    continue 'parse;
                                                }
                                                _ => {}
                                            }
                                            let response = if is_write_command(&cmd)
                                                && write_effects_armed(&tx, &state, &watch_registry)
                                            {
                                                let response = execute_and_record(&store, cmd.clone());
                                                apply_write_effects(&cmd, &response, &tx, 0, &state, &watch_registry, &store).await;
                                                response
                                            } else {
                                                execute_and_record(&store, cmd)
                                            };
                                            resp_buf.clear();
                                            response.serialize_into(&mut resp_buf);
                                            if writer.write_all(&resp_buf).await.is_err() {
                                                break 'outer;
                                            }
                                        }
                                    }
                                }
                                Err(ref e) if e == "Incomplete" => {
                                    // Compact: drop already-parsed bytes, reset cursor.
                                    buf.drain(..read_pos);
                                    read_pos = 0;
                                    break 'parse;
                                }
                                Err(e) => {
                                    warn!("TCP protocol error: {}", e);
                                    let _ = writer.write_all(b"-ERR Protocol error\r\n").await;
                                    buf.clear();
                                    read_pos = 0;
                                    break 'parse;
                                }
                            }
                        }
                        // Flush all responses for this read batch in one syscall.
                        if writer.flush().await.is_err() {
                            break 'outer;
                        }
                    }
                    Err(e) => {
                        warn!("TCP read error: {}", e);
                        break;
                    }
                }
            }

            msg = ps_rx.recv(), if is_subscribed => {
                match msg {
                    Some(m) => {
                        if writer.write_all(&encode_pubsub_msg(m)).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }

            // A watched key changed: mark the transaction dirty so a following
            // EXEC aborts. TCP clients get no keychange push (WATCH is pure CAS).
            notif = watch_rx.recv(), if !watched_keys.is_empty() => {
                if notif.is_some() {
                    watch_dirty = true;
                }
            }
        }
    }

    if !subscribed_channels.is_empty() || !subscribed_patterns.is_empty() {
        pubsub.lock().await.unsubscribe_all(conn_id);
    }
    unregister_all_watches(&watch_registry, conn_id, &mut watched_keys).await;
}

// ── WebSocket handler ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn handle_ws<S>(
    socket: S,
    store: Arc<KeyValueStore>,
    tx: broadcast::Sender<SyncMsg>,
    password: Arc<Option<String>>,
    conn_id: u64,
    pubsub: SharedPubSub,
    watch_registry: WatchRegistry,
    state: Arc<ServerState>,
    sync_secret: Arc<Option<String>>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let _guard = ConnectionGuard::ws();
    let ws_stream = match accept_async(socket).await {
        Ok(ws) => ws,
        Err(e) => {
            warn!("WS handshake failed on conn {}: {}", conn_id, e);
            return;
        }
    };

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    let mut rx = tx.subscribe();
    let mut is_authenticated = password.is_none();
    let mut auth_failures: u32 = 0;
    let mut multi_queue: Option<Vec<Command>> = None;
    let mut subscribed_channels: HashSet<String> = HashSet::new();
    let mut subscribed_patterns: HashSet<String> = HashSet::new();
    let (ps_tx, mut ps_rx) = mpsc::unbounded_channel::<PubSubMsg>();
    let mut watched_keys: HashSet<String> = HashSet::new();
    // Set when any watched key changes; EXEC aborts (returns nil) if true.
    let mut watch_dirty = false;
    let (watch_tx, mut watch_rx) = mpsc::unbounded_channel::<WatchNotif>();
    // Sync scopes for this connection. `strict` (RECACHED_SYNC_SECRET set)
    // means: no pushes and no key commands until a signed token is presented.
    // Without a secret, scopes are an opt-in bandwidth filter (legacy fan-out
    // of everything when None).
    let strict = sync_secret.is_some();
    let mut sync_scopes: Option<Vec<String>> = None;
    // Live-query subscriptions (QSUB). Keychange notifications for matching
    // keys arrive on their own channel so they never dirty WATCH transactions.
    let mut qsub_patterns: HashSet<String> = HashSet::new();
    let (q_tx, mut q_rx) = mpsc::unbounded_channel::<WatchNotif>();

    // NOTE: the WebSocket transport uses *text* frames, so values must be valid
    // UTF-8. Non-UTF-8 bytes are replaced (lossy) on the way out. This is safe
    // for the SDK, whose `set(key, value)` API only accepts `&str` values; raw
    // binary values are only fully round-trippable over the TCP (RESP) port.
    macro_rules! ws_send {
        ($bytes:expr) => {{
            let text = String::from_utf8_lossy($bytes).into_owned();
            if ws_sender.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }};
    }

    'outer: loop {
        let is_subscribed = !subscribed_channels.is_empty() || !subscribed_patterns.is_empty();

        tokio::select! {
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let (value, _) = match Value::parse(text.as_bytes()) {
                            Ok(v) => v,
                            Err(e) => {
                                let err = Value::Error(format!("ERR Protocol error: {}", e)).serialize();
                                ws_send!(&err);
                                continue;
                            }
                        };

                        let cmd = match Command::from_value(value) {
                            Ok(c) => c,
                            Err(e) => {
                                let err = Value::Error(e).serialize();
                                ws_send!(&err);
                                continue;
                            }
                        };

                        // AUTH
                        if let Command::Auth(ref pwd) = cmd {
                            let (disconnect, resp) = process_auth(
                                pwd, &password, &mut is_authenticated, &mut auth_failures,
                            );
                            ws_send!(&resp);
                            if disconnect { break; }
                            continue;
                        }

                        if !is_authenticated {
                            let resp = Value::Error("NOAUTH Authentication required.".to_string()).serialize();
                            ws_send!(&resp);
                            continue;
                        }

                        // ── Sync scoping ──────────────────────────────────────
                        if let Command::Sync(ref args) = cmd {
                            let resp = handle_sync_command(args, (*sync_secret).as_deref(), &mut sync_scopes, conn_id);
                            ws_send!(&resp);
                            continue;
                        }
                        // Token-scoped mode: check every command against this
                        // connection's granted scopes before it runs (including
                        // commands about to be queued inside MULTI).
                        if strict {
                            match command_scope(&cmd) {
                                CommandScope::KeyLess => {}
                                CommandScope::Admin => {
                                    ws_send!(b"-NOSCOPE keyspace-wide and administrative commands are not available on scoped WebSocket connections\r\n");
                                    continue;
                                }
                                CommandScope::Keys(keys) => {
                                    let Some(ref scopes) = sync_scopes else {
                                        ws_send!(b"-NOSCOPE send SYNC TOKEN <token> before issuing commands\r\n");
                                        continue;
                                    };
                                    if let Some(denied) = keys
                                        .iter()
                                        .find(|k| !scopes_match(scopes, std::slice::from_ref(k)))
                                    {
                                        let err = Value::Error(format!(
                                            "NOSCOPE key '{}' is outside this connection's sync scopes",
                                            denied
                                        ))
                                        .serialize();
                                        ws_send!(&err);
                                        continue;
                                    }
                                }
                            }
                        }

                        // ── Transactions ──────────────────────────────────────
                        match &cmd {
                            Command::Multi => {
                                let resp = if multi_queue.is_some() {
                                    b"-ERR MULTI calls can not be nested\r\n".to_vec()
                                } else {
                                    multi_queue = Some(Vec::new());
                                    b"+OK\r\n".to_vec()
                                };
                                ws_send!(&resp);
                                continue;
                            }
                            Command::Discard => {
                                let resp = if multi_queue.take().is_some() {
                                    // DISCARD also flushes WATCH state.
                                    unregister_all_watches(&watch_registry, conn_id, &mut watched_keys).await;
                                    while watch_rx.try_recv().is_ok() {} // drop stale notifications
                                    watch_dirty = false;
                                    b"+OK\r\n".to_vec()
                                } else {
                                    b"-ERR DISCARD without MULTI\r\n".to_vec()
                                };
                                ws_send!(&resp);
                                continue;
                            }
                            Command::Exec => {
                                match multi_queue.take() {
                                    None => {
                                        ws_send!(b"-ERR EXEC without MULTI\r\n");
                                    }
                                    Some(queue) => {
                                        // Catch watched-key changes that arrived but the select
                                        // loop hasn't drained yet, so the CAS check isn't racy.
                                        while watch_rx.try_recv().is_ok() {
                                            watch_dirty = true;
                                        }
                                        if watch_dirty {
                                            // A watched key changed since WATCH — abort: return
                                            // a nil array and run nothing (Redis CAS semantics).
                                            unregister_all_watches(&watch_registry, conn_id, &mut watched_keys).await;
                                            while watch_rx.try_recv().is_ok() {} // drop stale notifications
                                            watch_dirty = false;
                                            ws_send!(&Value::Array(None).serialize());
                                        } else {
                                            let mut results = Vec::with_capacity(queue.len());
                                            let armed = write_effects_armed(&tx, &state, &watch_registry);
                                            for qcmd in queue {
                                                let resp = if armed && is_write_command(&qcmd) {
                                                    let resp = execute_and_record(&store, qcmd.clone());
                                                    apply_write_effects(&qcmd, &resp, &tx, conn_id, &state, &watch_registry, &store).await;
                                                    resp
                                                } else {
                                                    execute_and_record(&store, qcmd)
                                                };
                                                results.push(resp);
                                            }
                                            // EXEC always flushes WATCH state. Drain any
                                            // self-notifications the queued writes produced so
                                            // they can't dirty a later transaction.
                                            unregister_all_watches(&watch_registry, conn_id, &mut watched_keys).await;
                                            while watch_rx.try_recv().is_ok() {}
                                            watch_dirty = false;
                                            let out = Value::Array(Some(results)).serialize();
                                            ws_send!(&out);
                                        }
                                    }
                                }
                                continue;
                            }
                            _ => {}
                        }

                        // Queue if inside MULTI
                        if let Some(ref mut queue) = multi_queue {
                            match &cmd {
                                Command::Subscribe(_) | Command::Unsubscribe(_)
                                | Command::PSubscribe(_) | Command::PUnsubscribe(_)
                                | Command::Publish(_, _)
                                | Command::Watch(_) | Command::Unwatch(_)
                                | Command::QSub(_) | Command::QUnsub(_) => {
                                    ws_send!(b"-ERR Command not allowed inside a transaction\r\n");
                                }
                                _ => {
                                    if queue.len() >= MAX_MULTI_QUEUE_LEN {
                                        ws_send!(b"-ERR transaction queue limit reached\r\n");
                                    } else {
                                        queue.push(cmd);
                                        ws_send!(b"+QUEUED\r\n");
                                    }
                                }
                            }
                            continue;
                        }

                        // ── Pub/Sub commands ──────────────────────────────────
                        match cmd {
                            Command::Subscribe(channels) => {
                                for ch in channels {
                                    subscribed_channels.insert(ch.clone());
                                    pubsub.lock().await.subscribe(conn_id, &ch, ps_tx.clone());
                                    let count = subscribed_channels.len() + subscribed_patterns.len();
                                    ws_send!(&resp_subscribe_ack("subscribe", &ch, count));
                                }
                            }
                            Command::Unsubscribe(channels) => {
                                let targets: Vec<String> = if channels.is_empty() {
                                    subscribed_channels.drain().collect()
                                } else {
                                    channels.into_iter().filter(|c| subscribed_channels.remove(c)).collect()
                                };
                                for ch in &targets {
                                    pubsub.lock().await.unsubscribe(conn_id, ch);
                                    let count = subscribed_channels.len() + subscribed_patterns.len();
                                    ws_send!(&resp_subscribe_ack("unsubscribe", ch, count));
                                }
                                if targets.is_empty() {
                                    ws_send!(&resp_subscribe_ack("unsubscribe", "", 0));
                                }
                            }
                            Command::PSubscribe(patterns) => {
                                for pat in patterns {
                                    subscribed_patterns.insert(pat.clone());
                                    pubsub.lock().await.psubscribe(conn_id, &pat, ps_tx.clone());
                                    let count = subscribed_channels.len() + subscribed_patterns.len();
                                    ws_send!(&resp_subscribe_ack("psubscribe", &pat, count));
                                }
                            }
                            Command::PUnsubscribe(patterns) => {
                                let targets: Vec<String> = if patterns.is_empty() {
                                    subscribed_patterns.drain().collect()
                                } else {
                                    patterns.into_iter().filter(|p| subscribed_patterns.remove(p)).collect()
                                };
                                for pat in &targets {
                                    pubsub.lock().await.punsubscribe(conn_id, pat);
                                    let count = subscribed_channels.len() + subscribed_patterns.len();
                                    ws_send!(&resp_subscribe_ack("punsubscribe", pat, count));
                                }
                                if targets.is_empty() {
                                    ws_send!(&resp_subscribe_ack("punsubscribe", "", 0));
                                }
                            }
                            Command::Publish(channel, message) => {
                                let count = pubsub.lock().await.publish(&channel, &message);
                                ws_send!(&Value::Integer(count).serialize());
                            }

                            Command::Watch(keys) => {
                                let new_count = keys
                                    .iter()
                                    .filter(|k| !watched_keys.contains(*k))
                                    .count();
                                if watched_keys.len() + new_count > MAX_WATCHES_PER_CONN {
                                    ws_send!(b"-ERR watch limit per connection reached\r\n");
                                } else {
                                    {
                                        let mut reg = watch_registry.map.lock().await;
                                        for key in &keys {
                                            if watched_keys.insert(key.clone()) {
                                                reg.entry(key.clone())
                                                    .or_default()
                                                    .push((conn_id, watch_tx.clone()));
                                            }
                                        }
                                        watch_registry.sync_len(&reg);
                                    } // reg dropped before await
                                    ws_send!(b"+OK\r\n");
                                }
                            }
                            Command::Unwatch(keys) => {
                                let targets: Vec<String> = if keys.is_empty() {
                                    watched_keys.drain().collect()
                                } else {
                                    keys.into_iter().filter(|k| watched_keys.remove(k)).collect()
                                };
                                {
                                    let mut reg = watch_registry.map.lock().await;
                                    for key in &targets {
                                        if let Some(subs) = reg.get_mut(key) {
                                            subs.retain(|(id, _)| *id != conn_id);
                                            if subs.is_empty() {
                                                reg.remove(key);
                                            }
                                        }
                                    }
                                    watch_registry.sync_len(&reg);
                                }
                                // Once nothing is watched, clear the dirty flag and drop any
                                // queued notifications so a later WATCH/MULTI/EXEC starts clean.
                                if watched_keys.is_empty() {
                                    while watch_rx.try_recv().is_ok() {}
                                    watch_dirty = false;
                                }
                                ws_send!(b"+OK\r\n");
                            }

                            Command::QSub(pattern) => {
                                // Strict mode: the requested pattern must sit inside a
                                // granted scope. A grant covers the request when it is
                                // identical or glob-matches the request as literal text
                                // (prefix-style grants: `cart:*` covers `cart:42:*`).
                                if strict {
                                    let allowed = sync_scopes.as_ref().is_some_and(|scopes| {
                                        scopes.iter().any(|s| {
                                            s == &pattern
                                                || core_engine::store::glob_match(s, &pattern)
                                        })
                                    });
                                    if !allowed {
                                        ws_send!(b"-NOSCOPE pattern is outside this connection's sync scopes\r\n");
                                        continue 'outer;
                                    }
                                }
                                if !qsub_patterns.contains(&pattern)
                                    && qsub_patterns.len() >= MAX_QSUBS_PER_CONN
                                {
                                    ws_send!(b"-ERR live query limit per connection reached\r\n");
                                    continue 'outer;
                                }
                                // Register *before* snapshotting: a write landing in
                                // between is delivered as a keychange after the initial
                                // state, which is idempotent — the reverse order would
                                // lose it.
                                if qsub_patterns.insert(pattern.clone()) {
                                    let mut pats = watch_registry.patterns.lock().await;
                                    pats.entry(pattern.clone())
                                        .or_default()
                                        .push((conn_id, q_tx.clone()));
                                    watch_registry.sync_patterns_len(&pats);
                                }
                                let kvs = store.matching_key_values(&pattern, MAX_QSUB_INITIAL_KEYS);
                                // Tagged reply so clients can recognise it among
                                // interleaved frames: ["qstate", pattern, k, v, ...]
                                let mut items = Vec::with_capacity(kvs.len() * 2 + 2);
                                items.push(Value::BulkString(Some(b"qstate".to_vec())));
                                items.push(Value::BulkString(Some(pattern.clone().into_bytes())));
                                for (k, v) in kvs {
                                    items.push(Value::BulkString(Some(k.into_bytes())));
                                    items.push(v);
                                }
                                ws_send!(&Value::Array(Some(items)).serialize());
                            }
                            Command::QUnsub(pattern) => {
                                let targets: Vec<String> = match pattern {
                                    Some(p) => {
                                        if qsub_patterns.remove(&p) {
                                            vec![p]
                                        } else {
                                            vec![]
                                        }
                                    }
                                    None => qsub_patterns.drain().collect(),
                                };
                                if !targets.is_empty() {
                                    let mut pats = watch_registry.patterns.lock().await;
                                    for p in &targets {
                                        if let Some(subs) = pats.get_mut(p) {
                                            subs.retain(|(id, _)| *id != conn_id);
                                            if subs.is_empty() {
                                                pats.remove(p);
                                            }
                                        }
                                    }
                                    watch_registry.sync_patterns_len(&pats);
                                }
                                ws_send!(b"+OK\r\n");
                            }

                            cmd => {
                                // Exactly-once: unwrap the DEDUP envelope. An id at or
                                // below this client's high-water mark was already applied
                                // (its acknowledgment was lost) — skip it. +DUP still
                                // acknowledges the write so the client retires it.
                                let cmd = match cmd {
                                    Command::Dedup(client, id, inner) => {
                                        if state.dedup_seen(&client, id) {
                                            ws_send!(b"+DUP\r\n");
                                            continue 'outer;
                                        }
                                        *inner
                                    }
                                    other => other,
                                };
                                if is_subscribed && !matches!(cmd, Command::Ping(_)) {
                                    ws_send!(b"-ERR only (P)SUBSCRIBE / (P)UNSUBSCRIBE / PING / QUIT allowed in subscribe mode\r\n");
                                    continue 'outer;
                                }
                                // Replica: reject writes
                                if state.is_replica() && is_write_command(&cmd) {
                                    ws_send!(b"-READONLY You can't write against a read only replica.\r\n");
                                    continue 'outer;
                                }
                                // Snapshot commands
                                match &cmd {
                                    Command::Save => {
                                        state.save(&store).await;
                                        ws_send!(b"+OK\r\n");
                                        continue 'outer;
                                    }
                                    Command::BgSave => {
                                        let s = Arc::clone(&store);
                                        let st = Arc::clone(&state);
                                        tokio::spawn(async move { st.save(&s).await; });
                                        ws_send!(b"+Background saving started\r\n");
                                        continue 'outer;
                                    }
                                    Command::LastSave => {
                                        let ts = state.snap.last_save.load(Ordering::Relaxed);
                                        ws_send!(&Value::Integer(ts).serialize());
                                        continue 'outer;
                                    }
                                    Command::ReplicaOfNoOne => {
                                        state.promote_to_primary();
                                        ws_send!(b"+OK\r\n");
                                        continue 'outer;
                                    }
                                    _ => {}
                                }
                                let response = if is_write_command(&cmd)
                                    && write_effects_armed(&tx, &state, &watch_registry)
                                {
                                    let response = execute_and_record(&store, cmd.clone());
                                    apply_write_effects(&cmd, &response, &tx, conn_id, &state, &watch_registry, &store).await;
                                    response
                                } else {
                                    execute_and_record(&store, cmd)
                                };
                                ws_send!(&response.serialize());
                            }
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        warn!("WS error on conn {}: {}", conn_id, e);
                        break;
                    }
                    None => break,
                }
            }

            result = rx.recv() => {
                match result {
                    Ok(push) if push.origin != conn_id => {
                        // Scope filter: with scopes set, only matching keys are
                        // forwarded. Without scopes, legacy mode forwards
                        // everything; strict mode forwards nothing until a
                        // token has been presented.
                        let visible = match &sync_scopes {
                            Some(scopes) => scopes_match(scopes, &push.keys),
                            None => !strict,
                        };
                        if visible
                            && ws_sender.send(Message::Text(push.resp.clone().into())).await.is_err()
                        {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("WS conn {} lagged, missed {} messages, resubscribing", conn_id, n);
                        rx = tx.subscribe();
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }

            msg = ps_rx.recv(), if is_subscribed => {
                match msg {
                    Some(m) => {
                        let bytes = encode_pubsub_msg(m);
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        if ws_sender.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }

            notif = watch_rx.recv(), if !watched_keys.is_empty() => {
                if let Some((key, value)) = notif {
                    // A watched key changed: mark the transaction dirty (so a
                    // following EXEC aborts) and still push the keychange to the
                    // client for the observable-keys feature.
                    watch_dirty = true;
                    let bytes = encode_keychange(&key, &value);
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    if ws_sender.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
            }

            // Live-query keychange: same frame as WATCH pushes, but never
            // dirties transactions.
            notif = q_rx.recv(), if !qsub_patterns.is_empty() => {
                if let Some((key, value)) = notif {
                    let bytes = encode_keychange(&key, &value);
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    if ws_sender.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    if !subscribed_channels.is_empty() || !subscribed_patterns.is_empty() {
        pubsub.lock().await.unsubscribe_all(conn_id);
    }
    if !watched_keys.is_empty() {
        let mut reg = watch_registry.map.lock().await;
        for key in &watched_keys {
            if let Some(subs) = reg.get_mut(key) {
                subs.retain(|(id, _)| *id != conn_id);
                if subs.is_empty() {
                    reg.remove(key);
                }
            }
        }
        watch_registry.sync_len(&reg);
    }
    unregister_all_qsubs(&watch_registry, conn_id, &mut qsub_patterns).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_engine::cmd::{SetOptions, ZAddOptions};
    use core_engine::resp::Value;
    use core_engine::store::KeyValueStore;
    use std::sync::atomic::{AtomicBool, AtomicI64};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("recached_test_{name}_{}", std::process::id()))
    }

    // ── TestServer harness ────────────────────────────────────────────────────

    struct TestServer {
        pub tcp_addr: std::net::SocketAddr,
        pub store: Arc<KeyValueStore>,
        pub state: Arc<ServerState>,
        _task: tokio::task::JoinHandle<()>,
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self._task.abort();
        }
    }

    async fn spawn_server() -> TestServer {
        spawn_server_cfg(None, None, false).await
    }

    async fn spawn_server_cfg(
        password: Option<&str>,
        snap_path: Option<PathBuf>,
        start_as_replica: bool,
    ) -> TestServer {
        let store = Arc::new(KeyValueStore::new());
        let (tx, _rx) = broadcast::channel::<SyncMsg>(256);
        let pubsub: SharedPubSub = Arc::new(tokio::sync::Mutex::new(PubSubHub::new()));
        let watch_registry: WatchRegistry = WatchHub::new();
        let semaphore = Arc::new(Semaphore::new(64));
        let snap_cfg = Arc::new(SnapshotConfig {
            path: snap_path.unwrap_or_else(|| tmp_path("test.rdb")),
            last_save: AtomicI64::new(now_unix_secs()),
        });
        let state = Arc::new(ServerState {
            snap: snap_cfg,
            aof: None,
            replicas: ReplHub::new(),
            is_replica: AtomicBool::new(start_as_replica),
            dedup: std::sync::Mutex::new(HashMap::new()),
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let store2 = Arc::clone(&store);
        let state2 = Arc::clone(&state);
        let pass = Arc::new(password.map(|s| s.to_string()));

        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let Ok(permit) = Arc::clone(&semaphore).try_acquire_owned() else {
                    continue;
                };
                let (s, t, p, ps, wr, st) = (
                    Arc::clone(&store2),
                    tx.clone(),
                    Arc::clone(&pass),
                    Arc::clone(&pubsub),
                    Arc::clone(&watch_registry),
                    Arc::clone(&state2),
                );
                tokio::spawn(async move {
                    handle_tcp(socket, s, t, p, ps, wr, st).await;
                    drop(permit);
                });
            }
        });

        TestServer {
            tcp_addr: addr,
            store,
            state,
            _task: task,
        }
    }

    // ── RespClient ────────────────────────────────────────────────────────────

    struct RespClient {
        stream: TcpStream,
        buf: Vec<u8>,
        filled: usize,
    }

    impl RespClient {
        async fn connect(addr: std::net::SocketAddr) -> Self {
            Self {
                stream: TcpStream::connect(addr).await.unwrap(),
                buf: vec![0u8; 65536],
                filled: 0,
            }
        }

        async fn cmd(&mut self, args: &[&str]) -> Value {
            let mut req = format!("*{}\r\n", args.len());
            for a in args {
                req.push_str(&format!("${}\r\n{}\r\n", a.len(), a));
            }
            self.stream.write_all(req.as_bytes()).await.unwrap();
            loop {
                match Value::parse(&self.buf[..self.filled]) {
                    Ok((val, n)) => {
                        self.buf.copy_within(n..self.filled, 0);
                        self.filled -= n;
                        return val;
                    }
                    Err(ref e) if e == "Incomplete" => {
                        let n = self
                            .stream
                            .read(&mut self.buf[self.filled..])
                            .await
                            .unwrap();
                        assert!(n > 0, "server closed connection unexpectedly");
                        self.filled += n;
                    }
                    Err(e) => panic!("RESP parse error: {e}"),
                }
            }
        }

        async fn read_until_closed(&mut self) {
            let mut buf = [0u8; 64];
            while self.stream.read(&mut buf).await.unwrap_or(0) > 0 {}
        }
    }

    fn ok() -> Value {
        Value::SimpleString("OK".to_string())
    }
    fn nil() -> Value {
        Value::BulkString(None)
    }
    fn bulk(s: &str) -> Value {
        Value::BulkString(Some(s.as_bytes().to_vec()))
    }
    fn int(n: i64) -> Value {
        Value::Integer(n)
    }
    fn arr(items: &[&str]) -> Value {
        Value::Array(Some(items.iter().map(|s| bulk(s)).collect()))
    }

    // ── is_write_command ──────────────────────────────────────────────────────

    #[test]
    fn is_write_command_classifies_correctly() {
        assert!(is_write_command(&Command::Set(
            "k".into(),
            "v".into(),
            SetOptions::default()
        )));
        assert!(is_write_command(&Command::Del(vec!["k".into()])));
        assert!(is_write_command(&Command::Incr("k".into())));
        assert!(is_write_command(&Command::FlushDb));
        assert!(is_write_command(&Command::HSet(
            "h".into(),
            vec![("f".into(), "v".into())]
        )));
        assert!(is_write_command(&Command::LPush(
            "l".into(),
            vec!["v".into()]
        )));
        assert!(is_write_command(&Command::SAdd(
            "s".into(),
            vec!["m".into()]
        )));
        assert!(is_write_command(&Command::ZAdd(
            "z".into(),
            ZAddOptions::default(),
            vec![(1.0, "m".into())]
        )));
        // reads
        assert!(!is_write_command(&Command::Get("k".into())));
        assert!(!is_write_command(&Command::HGet("h".into(), "f".into())));
        assert!(!is_write_command(&Command::LRange("l".into(), 0, -1)));
        assert!(!is_write_command(&Command::SMembers("s".into())));
        assert!(!is_write_command(&Command::DbSize));
        assert!(!is_write_command(&Command::Ping(None)));
        assert!(!is_write_command(&Command::Publish(
            "ch".into(),
            "msg".into()
        )));
    }

    // ── AOF replay ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn replay_aof_missing_file() {
        let store = KeyValueStore::new();
        let path = tmp_path("aof_missing");
        let count = replay_aof(&store, &path).await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn replay_aof_basic() {
        let store = KeyValueStore::new();
        let path = tmp_path("aof_basic.aof");
        let resp = "*3\r\n$3\r\nSET\r\n$3\r\nfoo\r\n$3\r\nbar\r\n\
                    *3\r\n$3\r\nSET\r\n$3\r\nbaz\r\n$3\r\nqux\r\n";
        tokio::fs::write(&path, resp.as_bytes()).await.unwrap();
        let count = replay_aof(&store, &path).await;
        assert_eq!(count, 2);
        assert_eq!(store.execute(Command::DbSize), Value::Integer(2));
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn replay_aof_push_frames() {
        // The live server records writes via `on_write`, which stores them in
        // RESP3 Push (`>`) form. Replay must accept those, not just `*` arrays.
        let store = KeyValueStore::new();
        let path = tmp_path("aof_push.aof");
        let resp = ">3\r\n$3\r\nSET\r\n$3\r\nfoo\r\n$3\r\nbar\r\n";
        tokio::fs::write(&path, resp.as_bytes()).await.unwrap();
        let count = replay_aof(&store, &path).await;
        assert_eq!(count, 1);
        assert_eq!(
            store.execute(Command::Get("foo".into())),
            Value::BulkString(Some(b"bar".to_vec()))
        );
        let _ = tokio::fs::remove_file(&path).await;
    }

    // ── Snapshot save / load ──────────────────────────────────────────────────

    #[tokio::test]
    async fn snapshot_save_and_load() {
        let store = KeyValueStore::new();
        store.execute(Command::Set(
            "hello".into(),
            "world".into(),
            SetOptions::default(),
        ));
        let path = tmp_path("snap.rdb");
        let cfg = Arc::new(SnapshotConfig {
            path: path.clone(),
            last_save: AtomicI64::new(0),
        });
        save_snapshot(&store, &cfg).await;
        assert!(path.exists());
        let store2 = KeyValueStore::new();
        let loaded = load_snapshot(&store2, &path).await;
        assert!(loaded);
        assert_eq!(
            store2.execute(Command::Get("hello".into())),
            Value::BulkString(Some(b"world".to_vec()))
        );
        let _ = tokio::fs::remove_file(&path).await;
    }

    // ── AofWriter append / truncate ───────────────────────────────────────────

    #[tokio::test]
    async fn aof_writer_append_and_truncate() {
        let path = tmp_path("aof_writer.aof");
        let aof = AofWriter::open(path.clone(), AofSync::No).await.unwrap();
        aof.append("*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n")
            .await;
        aof.flush().await;
        let len_before = tokio::fs::metadata(&path).await.unwrap().len();
        assert!(len_before > 0);
        aof.truncate().await;
        let len_after = tokio::fs::metadata(&path).await.unwrap().len();
        assert_eq!(len_after, 0);
        let _ = tokio::fs::remove_file(&path).await;
    }

    // ── Integration: 3a basic commands ───────────────────────────────────────

    #[tokio::test]
    async fn integration_set_get_del() {
        let srv = spawn_server().await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        assert_eq!(c.cmd(&["SET", "k", "v"]).await, ok());
        assert_eq!(c.cmd(&["GET", "k"]).await, bulk("v"));
        assert_eq!(c.cmd(&["GET", "missing"]).await, nil());
        assert_eq!(c.cmd(&["DEL", "k"]).await, int(1));
        assert_eq!(c.cmd(&["GET", "k"]).await, nil());
        assert_eq!(c.cmd(&["DEL", "k"]).await, int(0)); // already gone
    }

    #[tokio::test]
    async fn integration_incr_and_expiry() {
        let srv = spawn_server().await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        assert_eq!(c.cmd(&["SET", "n", "10"]).await, ok());
        assert_eq!(c.cmd(&["INCR", "n"]).await, int(11));
        assert_eq!(c.cmd(&["INCRBY", "n", "4"]).await, int(15));
        assert_eq!(c.cmd(&["DECR", "n"]).await, int(14));

        // TTL: set a key with 1-second expiry and verify TTL and eventual expiry
        assert_eq!(c.cmd(&["SET", "ex", "val", "EX", "1"]).await, ok());
        let ttl = c.cmd(&["TTL", "ex"]).await;
        assert!(matches!(ttl, Value::Integer(1) | Value::Integer(0)));
        tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
        assert_eq!(c.cmd(&["GET", "ex"]).await, nil());
    }

    #[tokio::test]
    async fn integration_string_commands() {
        let srv = spawn_server().await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        // APPEND + STRLEN
        assert_eq!(c.cmd(&["APPEND", "s", "hello"]).await, int(5));
        assert_eq!(c.cmd(&["APPEND", "s", " world"]).await, int(11));
        assert_eq!(c.cmd(&["STRLEN", "s"]).await, int(11));

        // GETSET
        assert_eq!(c.cmd(&["GETSET", "s", "new"]).await, bulk("hello world"));
        assert_eq!(c.cmd(&["GET", "s"]).await, bulk("new"));

        // SETNX
        assert_eq!(c.cmd(&["SETNX", "nx", "first"]).await, int(1));
        assert_eq!(c.cmd(&["SETNX", "nx", "second"]).await, int(0));
        assert_eq!(c.cmd(&["GET", "nx"]).await, bulk("first"));

        // SETEX
        assert_eq!(c.cmd(&["SETEX", "ex", "60", "val"]).await, ok());
        let ttl = c.cmd(&["TTL", "ex"]).await;
        assert!(matches!(ttl, Value::Integer(t) if t > 0 && t <= 60));

        // MSET / MGET
        assert_eq!(c.cmd(&["MSET", "a", "1", "b", "2", "c", "3"]).await, ok());
        let got = c.cmd(&["MGET", "a", "b", "c", "missing"]).await;
        assert_eq!(
            got,
            Value::Array(Some(vec![bulk("1"), bulk("2"), bulk("3"), nil()]))
        );
    }

    #[tokio::test]
    async fn integration_hash_commands() {
        let srv = spawn_server().await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        assert_eq!(c.cmd(&["HSET", "h", "f1", "v1", "f2", "v2"]).await, int(2));
        assert_eq!(c.cmd(&["HGET", "h", "f1"]).await, bulk("v1"));
        assert_eq!(c.cmd(&["HGET", "h", "missing"]).await, nil());
        assert_eq!(c.cmd(&["HLEN", "h"]).await, int(2));
        assert_eq!(c.cmd(&["HDEL", "h", "f1"]).await, int(1));
        assert_eq!(c.cmd(&["HLEN", "h"]).await, int(1));
        // HGETALL returns field-value pairs
        let all = c.cmd(&["HGETALL", "h"]).await;
        assert_eq!(all, Value::Array(Some(vec![bulk("f2"), bulk("v2")])));
    }

    #[tokio::test]
    async fn integration_list_commands() {
        let srv = spawn_server().await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        assert_eq!(c.cmd(&["RPUSH", "l", "a", "b", "c"]).await, int(3));
        assert_eq!(c.cmd(&["LPUSH", "l", "z"]).await, int(4));
        assert_eq!(c.cmd(&["LLEN", "l"]).await, int(4));
        assert_eq!(
            c.cmd(&["LRANGE", "l", "0", "-1"]).await,
            Value::Array(Some(vec![bulk("z"), bulk("a"), bulk("b"), bulk("c")]))
        );
        assert_eq!(c.cmd(&["LPOP", "l"]).await, bulk("z"));
        assert_eq!(c.cmd(&["RPOP", "l"]).await, bulk("c"));
        assert_eq!(c.cmd(&["LLEN", "l"]).await, int(2));
    }

    #[tokio::test]
    async fn integration_set_commands() {
        let srv = spawn_server().await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        assert_eq!(c.cmd(&["SADD", "s", "a", "b", "c"]).await, int(3));
        assert_eq!(c.cmd(&["SADD", "s", "a"]).await, int(0)); // duplicate
        assert_eq!(c.cmd(&["SCARD", "s"]).await, int(3));
        assert_eq!(c.cmd(&["SISMEMBER", "s", "b"]).await, int(1));
        assert_eq!(c.cmd(&["SISMEMBER", "s", "x"]).await, int(0));
        assert_eq!(c.cmd(&["SREM", "s", "a"]).await, int(1));
        assert_eq!(c.cmd(&["SCARD", "s"]).await, int(2));
    }

    #[tokio::test]
    async fn integration_zset_commands() {
        let srv = spawn_server().await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        assert_eq!(
            c.cmd(&["ZADD", "z", "1.5", "a", "2.5", "b", "3.0", "c"])
                .await,
            int(3)
        );
        assert_eq!(c.cmd(&["ZCARD", "z"]).await, int(3));
        assert_eq!(c.cmd(&["ZSCORE", "z", "b"]).await, bulk("2.5"));
        assert_eq!(c.cmd(&["ZRANK", "z", "a"]).await, int(0));
        assert_eq!(c.cmd(&["ZRANK", "z", "c"]).await, int(2));
        assert_eq!(
            c.cmd(&["ZRANGE", "z", "0", "-1", "WITHSCORES"]).await,
            Value::Array(Some(vec![
                bulk("a"),
                bulk("1.5"),
                bulk("b"),
                bulk("2.5"),
                bulk("c"),
                bulk("3"),
            ]))
        );
        assert_eq!(c.cmd(&["ZREM", "z", "b"]).await, int(1));
        assert_eq!(c.cmd(&["ZCARD", "z"]).await, int(2));
    }

    #[tokio::test]
    async fn integration_transactions_exec() {
        let srv = spawn_server().await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        assert_eq!(c.cmd(&["SET", "counter", "10"]).await, ok());
        assert_eq!(c.cmd(&["MULTI"]).await, ok());
        assert_eq!(
            c.cmd(&["SET", "counter", "20"]).await,
            Value::SimpleString("QUEUED".to_string())
        );
        assert_eq!(
            c.cmd(&["INCR", "counter"]).await,
            Value::SimpleString("QUEUED".to_string())
        );
        let res = c.cmd(&["EXEC"]).await;
        assert_eq!(res, Value::Array(Some(vec![ok(), int(21)])));
        assert_eq!(c.cmd(&["GET", "counter"]).await, bulk("21"));
    }

    #[tokio::test]
    async fn integration_transactions_discard() {
        let srv = spawn_server().await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        assert_eq!(c.cmd(&["SET", "key", "original"]).await, ok());
        assert_eq!(c.cmd(&["MULTI"]).await, ok());
        assert_eq!(
            c.cmd(&["DEL", "key"]).await,
            Value::SimpleString("QUEUED".to_string())
        );
        assert_eq!(c.cmd(&["DISCARD"]).await, ok());
        assert_eq!(c.cmd(&["GET", "key"]).await, bulk("original")); // DEL was discarded
    }

    #[tokio::test]
    async fn integration_unknown_command() {
        let srv = spawn_server().await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        let r = c.cmd(&["NOTACOMMAND", "arg"]).await;
        assert!(matches!(r, Value::Error(_)));
    }

    // ── Integration: 3b auth ──────────────────────────────────────────────────

    #[tokio::test]
    async fn integration_auth_blocks_unauthenticated() {
        let srv = spawn_server_cfg(Some("secret"), None, false).await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        let r = c.cmd(&["SET", "k", "v"]).await;
        assert!(matches!(&r, Value::Error(e) if e.contains("NOAUTH")));
    }

    #[tokio::test]
    async fn integration_auth_correct() {
        let srv = spawn_server_cfg(Some("secret"), None, false).await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        assert_eq!(c.cmd(&["AUTH", "secret"]).await, ok());
        assert_eq!(c.cmd(&["SET", "k", "v"]).await, ok());
        assert_eq!(c.cmd(&["GET", "k"]).await, bulk("v"));
    }

    #[tokio::test]
    async fn integration_auth_wrong_password_lockout() {
        let srv = spawn_server_cfg(Some("secret"), None, false).await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        // First 4 wrong attempts → "ERR invalid password"
        for _ in 0..4 {
            let r = c.cmd(&["AUTH", "wrong"]).await;
            assert!(matches!(&r, Value::Error(e) if e.contains("invalid")));
        }
        // 5th attempt hits MAX_AUTH_FAILURES → "too many" + server disconnects
        let r = c.cmd(&["AUTH", "wrong"]).await;
        assert!(matches!(&r, Value::Error(e) if e.contains("too many")));
        c.read_until_closed().await;
    }

    // ── Integration: 3c persistence ───────────────────────────────────────────

    #[tokio::test]
    async fn integration_save_and_reload() {
        let snap = tmp_path("integ_snap.rdb");
        let srv = spawn_server_cfg(None, Some(snap.clone()), false).await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        assert_eq!(c.cmd(&["SET", "hello", "world"]).await, ok());
        assert_eq!(c.cmd(&["SET", "foo", "bar"]).await, ok());
        assert_eq!(c.cmd(&["SAVE"]).await, ok());

        // Load into a fresh store
        let store2 = KeyValueStore::new();
        let loaded = load_snapshot(&store2, &snap).await;
        assert!(loaded);
        assert_eq!(
            store2.execute(Command::Get("hello".into())),
            Value::BulkString(Some(b"world".to_vec()))
        );
        assert_eq!(
            store2.execute(Command::Get("foo".into())),
            Value::BulkString(Some(b"bar".to_vec()))
        );
        let _ = tokio::fs::remove_file(&snap).await;
    }

    #[tokio::test]
    async fn integration_aof_replay() {
        let path = tmp_path("integ_aof.aof");
        let aof = AofWriter::open(path.clone(), AofSync::No).await.unwrap();
        let store = KeyValueStore::new();
        let snap_cfg = Arc::new(SnapshotConfig {
            path: tmp_path("integ_aof.rdb"),
            last_save: AtomicI64::new(0),
        });
        let state = Arc::new(ServerState {
            snap: snap_cfg,
            aof: Some(Arc::new(aof)),
            replicas: ReplHub::new(),
            is_replica: AtomicBool::new(false),
            dedup: std::sync::Mutex::new(HashMap::new()),
        });

        // Simulate writes captured by AOF
        state
            .on_write("*3\r\n$3\r\nSET\r\n$5\r\nhello\r\n$5\r\nworld\r\n")
            .await;
        state
            .on_write("*3\r\n$3\r\nSET\r\n$3\r\nfoo\r\n$3\r\nbar\r\n")
            .await;
        if let Some(ref a) = state.aof {
            a.flush().await;
        }

        // Replay into fresh store
        let store2 = KeyValueStore::new();
        let count = replay_aof(&store2, &path).await;
        assert_eq!(count, 2);
        assert_eq!(
            store2.execute(Command::Get("hello".into())),
            Value::BulkString(Some(b"world".to_vec()))
        );
        drop(store); // suppress unused warning
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn integration_dirty_counter() {
        let srv = spawn_server().await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        assert_eq!(srv.store.dirty_count(), 0);

        assert_eq!(c.cmd(&["SET", "a", "1"]).await, ok());
        assert_eq!(c.cmd(&["SET", "b", "2"]).await, ok());
        assert_eq!(srv.store.dirty_count(), 2);

        let last_save_before = srv.state.snap.last_save.load(Ordering::Relaxed);

        // Trigger a save — dirty resets to 0
        assert_eq!(c.cmd(&["SAVE"]).await, ok());
        assert_eq!(srv.store.dirty_count(), 0);

        // No new writes → save condition not met → last_save unchanged after 1s
        tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
        let last_save_after = srv.state.snap.last_save.load(Ordering::Relaxed);
        assert_eq!(last_save_before, last_save_after); // no autosave fired (no conditions configured)
    }

    // ── Integration: 3d replication ───────────────────────────────────────────

    #[tokio::test]
    async fn integration_replica_rejects_writes() {
        let srv = spawn_server_cfg(None, None, true).await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        let r = c.cmd(&["SET", "k", "v"]).await;
        assert!(matches!(&r, Value::Error(e) if e.contains("READONLY")));
        // Reads still work
        assert_eq!(c.cmd(&["GET", "k"]).await, nil());
    }

    #[tokio::test]
    async fn integration_replicaof_no_one_promotes() {
        let srv = spawn_server_cfg(None, None, true).await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        // Promote
        assert_eq!(c.cmd(&["REPLICAOF", "NO", "ONE"]).await, ok());
        // Now writes are accepted
        assert_eq!(c.cmd(&["SET", "k", "v"]).await, ok());
        assert_eq!(c.cmd(&["GET", "k"]).await, bulk("v"));
        assert!(!srv.state.is_replica());
    }

    #[tokio::test]
    async fn integration_replica_receives_write() {
        // Spawn primary with a separate replication listener on a random port
        let primary = spawn_server().await;
        let repl_registry: ReplRegistry = ReplHub::new();
        let snap_cfg = Arc::clone(&primary.state.snap);
        let primary_store = Arc::clone(&primary.store);
        let reg = Arc::clone(&repl_registry);

        // Replication listener — binds on port 0
        let repl_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let repl_port = repl_listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((socket, _)) = repl_listener.accept().await {
                let s = Arc::clone(&primary_store);
                let sc = Arc::clone(&snap_cfg);
                let r = Arc::clone(&reg);
                tokio::spawn(handle_replica(
                    socket,
                    s,
                    sc,
                    r,
                    None,
                    DEFAULT_REPL_CHANNEL_CAPACITY,
                ));
            }
        });

        // Also wire the repl_registry into the primary state so on_write fans out
        // We can't replace state.replicas (it's private), but handle_replica adds
        // itself to the registry it receives. We pass the same repl_registry to
        // on_write via a workaround: patch primary state's replicas after the fact
        // by passing the same Arc. Since ServerState.replicas is private in our
        // TestServer, we re-use the one we created.
        // ── Simpler approach: replace on_write path by sharing registry ──
        // Instead, wire it through the primary ServerState directly.
        // (In practice the TestServer shares state.replicas which starts empty;
        // handle_replica will push its sender into it when it connects.)
        // The trick: we need primary.state.replicas to point to our repl_registry.
        // Since TestServer.state is Arc<ServerState>, we can't replace it.
        // Use a fresh primary state that shares our registry.
        let primary2 = {
            let store = Arc::clone(&primary.store);
            let (tx, _rx) = broadcast::channel::<SyncMsg>(256);
            let pubsub: SharedPubSub = Arc::new(tokio::sync::Mutex::new(PubSubHub::new()));
            let wr: WatchRegistry = WatchHub::new();
            let sem = Arc::new(Semaphore::new(64));
            let snap = Arc::clone(&primary.state.snap);
            let state = Arc::new(ServerState {
                snap,
                aof: None,
                replicas: Arc::clone(&repl_registry),
                is_replica: AtomicBool::new(false),
                dedup: std::sync::Mutex::new(HashMap::new()),
            });
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let store2 = Arc::clone(&store);
            let state2 = Arc::clone(&state);
            let pass = Arc::new(None::<String>);
            let task = tokio::spawn(async move {
                loop {
                    let Ok((socket, _)) = listener.accept().await else {
                        return;
                    };
                    let Ok(permit) = Arc::clone(&sem).try_acquire_owned() else {
                        continue;
                    };
                    let (s, t, p, ps, wrr, st) = (
                        Arc::clone(&store2),
                        tx.clone(),
                        Arc::clone(&pass),
                        Arc::clone(&pubsub),
                        Arc::clone(&wr),
                        Arc::clone(&state2),
                    );
                    tokio::spawn(async move {
                        handle_tcp(socket, s, t, p, ps, wrr, st).await;
                        drop(permit);
                    });
                }
            });
            TestServer {
                tcp_addr: addr,
                store,
                state,
                _task: task,
            }
        };

        // Start replica
        let replica_store = Arc::new(KeyValueStore::new());
        let replica_state = Arc::new(ServerState {
            snap: Arc::new(SnapshotConfig {
                path: tmp_path("repl_snap.rdb"),
                last_save: AtomicI64::new(0),
            }),
            aof: None,
            replicas: ReplHub::new(),
            is_replica: AtomicBool::new(true),
            dedup: std::sync::Mutex::new(HashMap::new()),
        });
        let rs = Arc::clone(&replica_store);
        let rst = Arc::clone(&replica_state);
        let repl_addr = format!("127.0.0.1:{repl_port}");
        let rtx = broadcast::channel::<SyncMsg>(16).0;
        tokio::spawn(async move {
            run_repl_client(repl_addr, rs, rst, None, None, rtx).await;
        });

        // Give replica time to connect and receive initial snapshot
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

        // Write to primary2 (which uses the shared repl_registry)
        let mut c = RespClient::connect(primary2.tcp_addr).await;
        assert_eq!(c.cmd(&["SET", "replkey", "replval"]).await, ok());

        // Give replication fan-out time to arrive
        tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

        assert_eq!(
            replica_store.execute(Command::Get("replkey".into())),
            Value::BulkString(Some(b"replval".to_vec()))
        );
    }

    // ── Integration: 3e load (ignored in normal CI) ───────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn integration_concurrent_writers() {
        let srv = Arc::new(spawn_server().await);
        let addr = srv.tcp_addr;

        let tasks: Vec<_> = (0..50)
            .map(|task_id| {
                tokio::spawn(async move {
                    let mut c = RespClient::connect(addr).await;
                    for i in 0..100u32 {
                        let key = format!("t{task_id}_{i}");
                        let val = format!("v{i}");
                        assert_eq!(c.cmd(&["SET", &key, &val]).await, ok());
                        assert_eq!(c.cmd(&["GET", &key]).await, bulk(&val));
                    }
                })
            })
            .collect();

        for t in tasks {
            t.await.unwrap();
        }
        // All 50 × 100 keys should be present
        assert_eq!(srv.store.execute(Command::DbSize), Value::Integer(5000));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn integration_connection_limit() {
        // Small semaphore: only 3 concurrent connections
        let store = Arc::new(KeyValueStore::new());
        let (tx, _rx) = broadcast::channel::<SyncMsg>(16);
        let pubsub: SharedPubSub = Arc::new(tokio::sync::Mutex::new(PubSubHub::new()));
        let watch_registry: WatchRegistry = WatchHub::new();
        let semaphore = Arc::new(Semaphore::new(3));
        let state = Arc::new(ServerState {
            snap: Arc::new(SnapshotConfig {
                path: tmp_path("conn_limit.rdb"),
                last_save: AtomicI64::new(0),
            }),
            aof: None,
            replicas: ReplHub::new(),
            is_replica: AtomicBool::new(false),
            dedup: std::sync::Mutex::new(HashMap::new()),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let store2 = Arc::clone(&store);
        let state2 = Arc::clone(&state);
        let pass = Arc::new(None::<String>);

        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let Ok(permit) = Arc::clone(&semaphore).try_acquire_owned() else {
                    // Drop socket immediately — connection limit reached
                    drop(socket);
                    continue;
                };
                let (s, t, p, ps, wr, st) = (
                    Arc::clone(&store2),
                    tx.clone(),
                    Arc::clone(&pass),
                    Arc::clone(&pubsub),
                    Arc::clone(&watch_registry),
                    Arc::clone(&state2),
                );
                tokio::spawn(async move {
                    handle_tcp(socket, s, t, p, ps, wr, st).await;
                    drop(permit);
                });
            }
        });

        // Open 3 connections and hold them (just send PING and keep the socket open)
        let mut holders = Vec::new();
        for _ in 0..3 {
            let mut c = RespClient::connect(addr).await;
            assert_eq!(
                c.cmd(&["PING"]).await,
                Value::SimpleString("PONG".to_string())
            );
            holders.push(c);
        }

        // 4th connection: server drops it immediately, so read returns 0
        let mut overflow = TcpStream::connect(addr).await.unwrap();
        let mut buf = [0u8; 64];
        let n = overflow.read(&mut buf).await.unwrap_or(0);
        assert_eq!(n, 0, "4th connection should have been closed by server");

        drop(holders);
    }

    // ── Integration: 3f chaos (ignored in normal CI) ──────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn integration_kill_primary_mid_write() {
        let srv = Arc::new(spawn_server().await);
        let addr = srv.tcp_addr;

        // Start 20 concurrent writers
        let tasks: Vec<_> = (0..20)
            .map(|i| {
                tokio::spawn(async move {
                    // Connect; tolerate connection errors (server may die mid-flight)
                    let stream = TcpStream::connect(addr).await;
                    if stream.is_err() {
                        return;
                    }
                    let mut c = RespClient {
                        stream: stream.unwrap(),
                        buf: vec![0u8; 65536],
                        filled: 0,
                    };
                    for j in 0..50u32 {
                        let key = format!("chaos_{i}_{j}");
                        // Ignore errors — server may die during this
                        let _ = tokio::time::timeout(
                            tokio::time::Duration::from_millis(200),
                            c.cmd(&["SET", &key, "v"]),
                        )
                        .await;
                    }
                })
            })
            .collect();

        // Kill the server after 10ms
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        srv._task.abort();

        // Join all writers — none should panic
        for t in tasks {
            let _ = t.await;
        }

        // Store is still intact in memory — no panic is the meaningful assertion here;
        // zero keys is valid if the server was killed before any write landed.
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn integration_failover_promotes() {
        // Point replica at a port that refuses connections immediately so the
        // unreachable timer starts on the first loop iteration without any
        // real primary required.  Promotion happens after:
        //   connect fail (fast) → backoff 2s → connect fail → elapsed ≥ 1s → promote
        // so we wait 3s to be safe.
        let replica_state = Arc::new(ServerState {
            snap: Arc::new(SnapshotConfig {
                path: tmp_path("failover_snap.rdb"),
                last_save: AtomicI64::new(0),
            }),
            aof: None,
            replicas: ReplHub::new(),
            is_replica: AtomicBool::new(true),
            dedup: std::sync::Mutex::new(HashMap::new()),
        });
        let replica_store = Arc::new(KeyValueStore::new());
        let rs = Arc::clone(&replica_store);
        let rst = Arc::clone(&replica_state);
        // Bind a listener then immediately drop it so the port is known-refused
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        drop(listener);
        let rtx = broadcast::channel::<SyncMsg>(16).0;
        tokio::spawn(async move {
            run_repl_client(dead_addr, rs, rst, None, Some(1), rtx).await;
        });

        // Wait for 2 backoff cycles (initial fail + 2s sleep + retry fail → promote)
        tokio::time::sleep(tokio::time::Duration::from_millis(3000)).await;

        assert!(
            !replica_state.is_replica(),
            "replica should have promoted after primary was unreachable for >1s"
        );
    }

    // ── WebSocket WATCH/EXEC harness ──────────────────────────────────────────

    /// Spawn a WebSocket server sharing one store + watch registry across all
    /// connections, so WATCH notifications fan out between clients.
    async fn spawn_ws_server() -> TestServer {
        spawn_ws_server_cfg(None).await
    }

    /// Like `spawn_ws_server`, with an optional sync-scope secret (strict mode).
    async fn spawn_ws_server_cfg(sync_secret: Option<String>) -> TestServer {
        let store = Arc::new(KeyValueStore::new());
        let (tx, _rx) = broadcast::channel::<SyncMsg>(256);
        let pubsub: SharedPubSub = Arc::new(tokio::sync::Mutex::new(PubSubHub::new()));
        let watch_registry: WatchRegistry = WatchHub::new();
        let snap_cfg = Arc::new(SnapshotConfig {
            path: tmp_path("ws_test.rdb"),
            last_save: AtomicI64::new(now_unix_secs()),
        });
        let state = Arc::new(ServerState {
            snap: snap_cfg,
            aof: None,
            replicas: ReplHub::new(),
            is_replica: AtomicBool::new(false),
            dedup: std::sync::Mutex::new(HashMap::new()),
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let store2 = Arc::clone(&store);
        let state2 = Arc::clone(&state);
        let secret = Arc::new(sync_secret);

        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let (s, t, ps, wr, st, ss) = (
                    Arc::clone(&store2),
                    tx.clone(),
                    Arc::clone(&pubsub),
                    Arc::clone(&watch_registry),
                    Arc::clone(&state2),
                    Arc::clone(&secret),
                );
                let id = next_conn_id();
                tokio::spawn(async move {
                    handle_ws(socket, s, t, Arc::new(None), id, ps, wr, st, ss).await;
                });
            }
        });

        TestServer {
            tcp_addr: addr,
            store,
            state,
            _task: task,
        }
    }

    struct WsClient {
        ws: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    }

    impl WsClient {
        async fn connect(addr: std::net::SocketAddr) -> Self {
            let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
                .await
                .unwrap();
            Self { ws }
        }

        async fn cmd(&mut self, args: &[&str]) -> Value {
            let mut req = format!("*{}\r\n", args.len());
            for a in args {
                req.push_str(&format!("${}\r\n{}\r\n", a.len(), a));
            }
            self.ws.send(Message::Text(req.into())).await.unwrap();
            self.next_reply().await
        }

        /// Wait up to `ms` for the next RESP3 Push broadcast frame, returning
        /// its raw text. `None` when nothing arrives in time.
        async fn recv_push(&mut self, ms: u64) -> Option<String> {
            let fut = async {
                loop {
                    match self.ws.next().await {
                        Some(Ok(Message::Text(t))) => {
                            let Ok((v, _)) = Value::parse(t.as_bytes()) else {
                                continue;
                            };
                            if matches!(v, Value::Push(_)) {
                                return Some(t.to_string());
                            }
                        }
                        Some(Ok(_)) => continue,
                        _ => return None,
                    }
                }
            };
            tokio::time::timeout(tokio::time::Duration::from_millis(ms), fut)
                .await
                .ok()
                .flatten()
        }

        /// Wait up to `ms` for the next `keychange` frame (WATCH / live-query
        /// push), returning `(key, value)`. `None` when nothing arrives.
        async fn recv_keychange(&mut self, ms: u64) -> Option<(String, Value)> {
            let fut = async {
                loop {
                    match self.ws.next().await {
                        Some(Ok(Message::Text(t))) => {
                            let Ok((v, _)) = Value::parse(t.as_bytes()) else {
                                continue;
                            };
                            if let Value::Array(Some(items)) = &v
                                && items.len() == 3
                                && matches!(items.first(), Some(Value::BulkString(Some(k))) if k == b"keychange")
                            {
                                let Value::BulkString(Some(key)) = &items[1] else {
                                    continue;
                                };
                                return Some((
                                    String::from_utf8_lossy(key).into_owned(),
                                    items[2].clone(),
                                ));
                            }
                        }
                        Some(Ok(_)) => continue,
                        _ => return None,
                    }
                }
            };
            tokio::time::timeout(tokio::time::Duration::from_millis(ms), fut)
                .await
                .ok()
                .flatten()
        }

        /// Read the next *command reply*, skipping server-initiated frames
        /// (RESP3 Push broadcasts and `keychange` observable-key pushes).
        async fn next_reply(&mut self) -> Value {
            loop {
                match self.ws.next().await {
                    Some(Ok(Message::Text(t))) => {
                        let Ok((v, _)) = Value::parse(t.as_bytes()) else {
                            continue;
                        };
                        if matches!(v, Value::Push(_)) {
                            continue;
                        }
                        if let Value::Array(Some(items)) = &v
                            && matches!(items.first(), Some(Value::BulkString(Some(k))) if k == b"keychange")
                        {
                            continue;
                        }
                        return v;
                    }
                    Some(Ok(_)) => continue,
                    _ => panic!("ws closed unexpectedly"),
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_ws_watch_exec_aborts_on_change() {
        let srv = spawn_ws_server().await;
        let mut watcher = WsClient::connect(srv.tcp_addr).await;
        let mut writer = WsClient::connect(srv.tcp_addr).await;

        assert_eq!(watcher.cmd(&["SET", "k", "v0"]).await, ok());
        assert_eq!(watcher.cmd(&["WATCH", "k"]).await, ok());

        // Another client mutates the watched key.
        assert_eq!(writer.cmd(&["SET", "k", "v1"]).await, ok());
        // Give the notification time to reach the watcher's registry channel.
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        assert_eq!(
            watcher.cmd(&["MULTI"]).await,
            Value::SimpleString("OK".into())
        );
        assert_eq!(
            watcher.cmd(&["SET", "k", "v2"]).await,
            Value::SimpleString("QUEUED".into())
        );
        // EXEC must abort with a nil array because k changed since WATCH.
        assert_eq!(watcher.cmd(&["EXEC"]).await, Value::Array(None));
        // The transaction did not run.
        assert_eq!(srv.store.execute(Command::Get("k".into())), bulk("v1"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_ws_watch_exec_runs_when_unchanged() {
        let srv = spawn_ws_server().await;
        let mut c = WsClient::connect(srv.tcp_addr).await;

        assert_eq!(c.cmd(&["WATCH", "k"]).await, ok());
        assert_eq!(c.cmd(&["MULTI"]).await, ok());
        assert_eq!(
            c.cmd(&["SET", "k", "v1"]).await,
            Value::SimpleString("QUEUED".into())
        );
        // No one touched k → EXEC runs and returns the queued results.
        assert_eq!(
            c.cmd(&["EXEC"]).await,
            Value::Array(Some(vec![Value::SimpleString("OK".into())]))
        );
        assert_eq!(srv.store.execute(Command::Get("k".into())), bulk("v1"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_tcp_watch_exec_aborts_on_change() {
        let srv = spawn_server().await;
        let mut watcher = RespClient::connect(srv.tcp_addr).await;
        let mut writer = RespClient::connect(srv.tcp_addr).await;

        assert_eq!(watcher.cmd(&["SET", "k", "v0"]).await, ok());
        assert_eq!(watcher.cmd(&["WATCH", "k"]).await, ok());
        // Another client mutates the watched key (reply awaited → notification queued).
        assert_eq!(writer.cmd(&["SET", "k", "v1"]).await, ok());

        assert_eq!(watcher.cmd(&["MULTI"]).await, ok());
        assert_eq!(
            watcher.cmd(&["SET", "k", "v2"]).await,
            Value::SimpleString("QUEUED".into())
        );
        // k changed since WATCH → EXEC aborts with a nil array.
        assert_eq!(watcher.cmd(&["EXEC"]).await, Value::Array(None));
        assert_eq!(watcher.cmd(&["GET", "k"]).await, bulk("v1"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_tcp_watch_exec_runs_when_unchanged() {
        let srv = spawn_server().await;
        let mut c = RespClient::connect(srv.tcp_addr).await;

        assert_eq!(c.cmd(&["WATCH", "k"]).await, ok());
        assert_eq!(c.cmd(&["MULTI"]).await, ok());
        assert_eq!(
            c.cmd(&["SET", "k", "v1"]).await,
            Value::SimpleString("QUEUED".into())
        );
        assert_eq!(
            c.cmd(&["EXEC"]).await,
            Value::Array(Some(vec![Value::SimpleString("OK".into())]))
        );
        assert_eq!(c.cmd(&["GET", "k"]).await, bulk("v1"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_tcp_watch_inside_multi_rejected() {
        let srv = spawn_server().await;
        let mut c = RespClient::connect(srv.tcp_addr).await;
        assert_eq!(c.cmd(&["MULTI"]).await, ok());
        // WATCH is not allowed once a transaction has started.
        assert!(matches!(c.cmd(&["WATCH", "k"]).await, Value::Error(_)));
    }

    // ── Sync scoping ──────────────────────────────────────────────────────────

    /// Mint a sync-scope token the way an application backend would:
    /// HMAC-SHA256 over the base64url payload text.
    fn mint_sync_token(secret: &str, payload: &str) -> String {
        use base64::Engine as _;
        use hmac::{Hmac, Mac};
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let payload_b64 = engine.encode(payload);
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload_b64.as_bytes());
        let sig = engine.encode(mac.finalize().into_bytes());
        format!("{payload_b64}.{sig}")
    }

    #[test]
    fn sync_token_roundtrip_and_rejections() {
        let tok = mint_sync_token("s3cret", "cart:42:*,profile:42");
        assert_eq!(
            verify_sync_token("s3cret", &tok).unwrap(),
            vec!["cart:42:*".to_string(), "profile:42".to_string()]
        );
        // Wrong secret → invalid signature.
        assert_eq!(
            verify_sync_token("other", &tok).unwrap_err(),
            "invalid signature"
        );
        // Tampered payload → invalid signature.
        let (_, sig) = tok.split_once('.').unwrap();
        use base64::Engine as _;
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let forged = format!("{}.{}", engine.encode("admin:*"), sig);
        assert_eq!(
            verify_sync_token("s3cret", &forged).unwrap_err(),
            "invalid signature"
        );
        // Expired token.
        let expired = mint_sync_token("s3cret", "cart:*|1");
        assert_eq!(
            verify_sync_token("s3cret", &expired).unwrap_err(),
            "token expired"
        );
        // Future expiry still valid.
        let live = mint_sync_token("s3cret", "cart:*|99999999999");
        assert!(verify_sync_token("s3cret", &live).is_ok());
        // Empty patterns / malformed.
        let empty = mint_sync_token("s3cret", "");
        assert_eq!(
            verify_sync_token("s3cret", &empty).unwrap_err(),
            "token grants no patterns"
        );
        assert!(verify_sync_token("s3cret", "no-dot-here").is_err());
    }

    #[test]
    fn scopes_match_globs_and_flushdb() {
        let scopes = vec!["cart:42:*".to_string(), "catalog:*".to_string()];
        assert!(scopes_match(&scopes, &["cart:42:item:1".to_string()]));
        assert!(scopes_match(&scopes, &["catalog:books".to_string()]));
        assert!(!scopes_match(&scopes, &["cart:7:item:1".to_string()]));
        assert!(!scopes_match(&scopes, &["session:42".to_string()]));
        // Multi-key: any matching key makes the push visible.
        assert!(scopes_match(
            &scopes,
            &["session:42".to_string(), "catalog:books".to_string()]
        ));
        // No keys = FLUSHDB — visible to every scope.
        assert!(scopes_match(&scopes, &[]));
    }

    #[test]
    fn command_scope_classification() {
        assert!(matches!(
            command_scope(&Command::Ping(None)),
            CommandScope::KeyLess
        ));
        assert!(matches!(
            command_scope(&Command::Keys("*".into())),
            CommandScope::Admin
        ));
        assert!(matches!(
            command_scope(&Command::FlushDb),
            CommandScope::Admin
        ));
        match command_scope(&Command::Get("a".into())) {
            CommandScope::Keys(k) => assert_eq!(k, vec!["a".to_string()]),
            _ => panic!("GET should be key-scoped"),
        }
        match command_scope(&Command::SInterStore(
            "dst".into(),
            vec!["a".into(), "b".into()],
        )) {
            CommandScope::Keys(k) => {
                assert!(k.contains(&"dst".to_string()) && k.contains(&"a".to_string()))
            }
            _ => panic!("SINTERSTORE should be key-scoped"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_ws_sync_scope_filters_fanout() {
        let srv = spawn_ws_server().await;
        let mut scoped = WsClient::connect(srv.tcp_addr).await;
        let mut unscoped = WsClient::connect(srv.tcp_addr).await;
        let mut writer = WsClient::connect(srv.tcp_addr).await;

        // Open mode: SYNC with literal patterns.
        assert_eq!(scoped.cmd(&["SYNC", "cart:*"]).await, arr(&["cart:*"]));

        assert_eq!(writer.cmd(&["SET", "cart:1", "x"]).await, ok());
        assert_eq!(writer.cmd(&["SET", "other:1", "y"]).await, ok());

        // Scoped client sees the cart write and nothing else.
        let push = scoped.recv_push(1000).await.expect("expected cart:1 push");
        assert!(push.contains("cart:1"), "unexpected push: {push}");
        assert!(
            scoped.recv_push(300).await.is_none(),
            "out-of-scope push leaked to scoped client"
        );

        // Unscoped client (legacy mode) sees both.
        let p1 = unscoped.recv_push(1000).await.expect("push 1");
        let p2 = unscoped.recv_push(1000).await.expect("push 2");
        assert!(p1.contains("cart:1") && p2.contains("other:1"));

        // Bare SYNC reports current scopes.
        assert_eq!(scoped.cmd(&["SYNC"]).await, arr(&["cart:*"]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_ws_sync_strict_mode_gates_and_filters() {
        let secret = "integration-secret";
        let srv = spawn_ws_server_cfg(Some(secret.to_string())).await;
        let mut client = WsClient::connect(srv.tcp_addr).await;

        // No token yet: key commands and pushes are refused.
        let r = client.cmd(&["GET", "cart:1"]).await;
        assert!(matches!(&r, Value::Error(e) if e.contains("NOSCOPE")));
        // Literal patterns are rejected in strict mode.
        let r = client.cmd(&["SYNC", "cart:*"]).await;
        assert!(matches!(&r, Value::Error(e) if e.contains("signed scopes")));
        // Garbage token.
        let r = client.cmd(&["SYNC", "TOKEN", "not-a-token"]).await;
        assert!(matches!(&r, Value::Error(e) if e.contains("invalid sync token")));

        // Valid token: scoped to cart:* only.
        let tok = mint_sync_token(secret, "cart:*");
        assert_eq!(client.cmd(&["SYNC", "TOKEN", &tok]).await, arr(&["cart:*"]));

        // In-scope commands work; out-of-scope and admin are refused.
        assert_eq!(client.cmd(&["SET", "cart:1", "x"]).await, ok());
        assert_eq!(client.cmd(&["GET", "cart:1"]).await, bulk("x"));
        let r = client.cmd(&["GET", "secret-key"]).await;
        assert!(matches!(&r, Value::Error(e) if e.contains("NOSCOPE")));
        let r = client.cmd(&["KEYS", "*"]).await;
        assert!(matches!(&r, Value::Error(e) if e.contains("NOSCOPE")));

        // Fan-out: a second scoped client writes in and out of the first's scope.
        let mut writer = WsClient::connect(srv.tcp_addr).await;
        let wtok = mint_sync_token(secret, "cart:*,other:*");
        assert_eq!(
            writer.cmd(&["SYNC", "TOKEN", &wtok]).await,
            arr(&["cart:*", "other:*"])
        );
        assert_eq!(writer.cmd(&["SET", "cart:2", "a"]).await, ok());
        assert_eq!(writer.cmd(&["SET", "other:2", "b"]).await, ok());

        let push = client.recv_push(1000).await.expect("expected cart:2 push");
        assert!(push.contains("cart:2"), "unexpected push: {push}");
        assert!(
            client.recv_push(300).await.is_none(),
            "out-of-scope push leaked on strict connection"
        );
    }

    // ── Exactly-once delivery (DEDUP) ─────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_dedup_skips_replayed_writes() {
        let srv = spawn_ws_server().await;
        let mut c = WsClient::connect(srv.tcp_addr).await;

        // First delivery applies.
        assert_eq!(
            c.cmd(&["DEDUP", "client-a", "1", "INCRBY", "n", "2"]).await,
            int(2)
        );
        // Exact replay (ack lost, client re-sent) is skipped.
        assert_eq!(
            c.cmd(&["DEDUP", "client-a", "1", "INCRBY", "n", "2"]).await,
            Value::SimpleString("DUP".into())
        );
        // Higher id applies.
        assert_eq!(
            c.cmd(&["DEDUP", "client-a", "2", "INCRBY", "n", "3"]).await,
            int(5)
        );
        // The high-water mark survives a reconnect — the whole point.
        let mut c2 = WsClient::connect(srv.tcp_addr).await;
        assert_eq!(
            c2.cmd(&["DEDUP", "client-a", "2", "INCRBY", "n", "3"])
                .await,
            Value::SimpleString("DUP".into())
        );
        assert_eq!(srv.store.execute(Command::Get("n".into())), bulk("5"));
        // A different client id has an independent mark.
        assert_eq!(
            c2.cmd(&["DEDUP", "client-b", "1", "INCRBY", "n", "1"])
                .await,
            int(6)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_dedup_respects_sync_scopes() {
        let secret = "dedup-secret";
        let srv = spawn_ws_server_cfg(Some(secret.to_string())).await;
        let mut c = WsClient::connect(srv.tcp_addr).await;
        let tok = mint_sync_token(secret, "cart:*");
        assert_eq!(c.cmd(&["SYNC", "TOKEN", &tok]).await, arr(&["cart:*"]));

        // Scope enforcement applies to the wrapped command.
        assert_eq!(
            c.cmd(&["DEDUP", "c1", "1", "SET", "cart:1", "x"]).await,
            ok()
        );
        let r = c.cmd(&["DEDUP", "c1", "2", "SET", "admin:1", "x"]).await;
        assert!(matches!(&r, Value::Error(e) if e.contains("NOSCOPE")));
    }

    // ── JSON over the wire ────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_json_commands_and_fanout() {
        let srv = spawn_ws_server().await;
        let mut writer = WsClient::connect(srv.tcp_addr).await;
        let mut peer = WsClient::connect(srv.tcp_addr).await;

        assert_eq!(
            writer.cmd(&["JSET", "doc:1", "$", r#"{"a":1}"#]).await,
            ok()
        );
        assert_eq!(writer.cmd(&["JGET", "doc:1", "$.a"]).await, bulk("1"));
        assert_eq!(
            writer
                .cmd(&["JMERGE", "doc:1", r#"{"b":2,"a":null}"#])
                .await,
            ok()
        );
        assert_eq!(writer.cmd(&["JGET", "doc:1"]).await, bulk(r#"{"b":2}"#));

        // Peers receive the writes as replayable pushes.
        let p = peer.recv_push(1000).await.expect("JSET push");
        assert!(p.contains("JSET") && p.contains("doc:1"), "push: {p}");
        let p2 = peer.recv_push(1000).await.expect("JMERGE push");
        assert!(p2.contains("JMERGE"), "push: {p2}");

        // Failed writes are not broadcast.
        let r = writer.cmd(&["JSET", "doc:1", "$", "{bad"]).await;
        assert!(matches!(&r, Value::Error(e) if e.contains("invalid JSON")));
        assert!(peer.recv_push(300).await.is_none());
    }

    // ── Live queries (QSUB / QUNSUB) ──────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_ws_qsub_initial_state_and_diffs() {
        let srv = spawn_ws_server().await;
        let mut writer = WsClient::connect(srv.tcp_addr).await;
        let mut client = WsClient::connect(srv.tcp_addr).await;

        // Pre-existing state the subscription must deliver up front.
        assert_eq!(writer.cmd(&["SET", "cart:1", "apples"]).await, ok());
        assert_eq!(writer.cmd(&["SET", "other:1", "zzz"]).await, ok());

        let initial = client.cmd(&["QSUB", "cart:*"]).await;
        match &initial {
            Value::Array(Some(items)) => {
                assert_eq!(
                    items.len(),
                    4,
                    "expected tag + pattern + one pair: {items:?}"
                );
                assert_eq!(items[0], bulk("qstate"));
                assert_eq!(items[1], bulk("cart:*"));
                assert_eq!(items[2], bulk("cart:1"));
                assert_eq!(items[3], bulk("apples"));
            }
            other => panic!("expected initial-state array, got {other:?}"),
        }

        // A matching write arrives as a keychange diff…
        assert_eq!(writer.cmd(&["SET", "cart:2", "pears"]).await, ok());
        let (key, value) = client.recv_keychange(1000).await.expect("cart:2 diff");
        assert_eq!((key.as_str(), &value), ("cart:2", &bulk("pears")));

        // …a non-matching write does not…
        assert_eq!(writer.cmd(&["SET", "other:2", "yyy"]).await, ok());
        assert!(client.recv_keychange(300).await.is_none());

        // …a deletion arrives as a nil keychange…
        assert_eq!(writer.cmd(&["DEL", "cart:2"]).await, int(1));
        let (key, value) = client.recv_keychange(1000).await.expect("delete diff");
        assert_eq!((key.as_str(), &value), ("cart:2", &nil()));

        // …and QUNSUB stops the stream.
        assert_eq!(client.cmd(&["QUNSUB", "cart:*"]).await, ok());
        assert_eq!(writer.cmd(&["SET", "cart:3", "plums"]).await, ok());
        assert!(client.recv_keychange(300).await.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn integration_ws_qsub_strict_scope() {
        let secret = "qsub-secret";
        let srv = spawn_ws_server_cfg(Some(secret.to_string())).await;
        let mut client = WsClient::connect(srv.tcp_addr).await;

        let tok = mint_sync_token(secret, "cart:*");
        assert_eq!(client.cmd(&["SYNC", "TOKEN", &tok]).await, arr(&["cart:*"]));

        // A narrower pattern under the grant is allowed (prefix-style cover).
        assert_eq!(
            client.cmd(&["QSUB", "cart:42:*"]).await,
            arr(&["qstate", "cart:42:*"])
        );
        // A pattern outside the grant is refused.
        let r = client.cmd(&["QSUB", "admin:*"]).await;
        assert!(matches!(&r, Value::Error(e) if e.contains("NOSCOPE")));

        // Diffs flow for the subscribed pattern.
        let mut writer = WsClient::connect(srv.tcp_addr).await;
        let wtok = mint_sync_token(secret, "cart:*");
        writer.cmd(&["SYNC", "TOKEN", &wtok]).await;
        assert_eq!(writer.cmd(&["SET", "cart:42:item", "x"]).await, ok());
        let (key, _) = client.recv_keychange(1000).await.expect("scoped diff");
        assert_eq!(key, "cart:42:item");
    }
}
