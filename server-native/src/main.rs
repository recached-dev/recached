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
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
        Command::Unknown(_) => "unknown",
    }
}

fn execute_and_record(store: &KeyValueStore, cmd: &Command) -> Value {
    let name = command_name(cmd);
    let response = store.execute(cmd.clone());
    counter!("recached_commands_total", "command" => name).increment(1);
    if matches!(response, Value::Error(_)) {
        counter!("recached_command_errors_total", "command" => name).increment(1);
    }
    if matches!(cmd, Command::Get(_)) {
        match &response {
            Value::BulkString(Some(_)) => counter!("recached_keyspace_hits_total").increment(1),
            Value::BulkString(None) => counter!("recached_keyspace_misses_total").increment(1),
            _ => {}
        }
    }
    response
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

const TCP_READ_BUFFER_BYTES: usize = 4096;
const MAX_TCP_READ_BUFFER_BYTES: usize = 64 * 1024 * 1024; // 64 MB per connection
const MAX_MULTI_QUEUE_LEN: usize = 10_000;
const MAX_WATCHES_PER_CONN: usize = 1_024;
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
                if let Ok(cmd) = Command::from_value(value) {
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

type ReplSender = mpsc::UnboundedSender<Vec<u8>>;
type ReplRegistry = Arc<tokio::sync::Mutex<Vec<ReplSender>>>;

// ── Server state ──────────────────────────────────────────────────────────────

struct ServerState {
    snap: Arc<SnapshotConfig>,
    aof: Option<Arc<AofWriter>>,
    replicas: ReplRegistry,
    /// true = currently acting as a read-only replica
    is_replica: std::sync::atomic::AtomicBool,
}

impl ServerState {
    fn is_replica(&self) -> bool {
        self.is_replica.load(Ordering::Relaxed)
    }

    fn promote_to_primary(&self) {
        self.is_replica.store(false, Ordering::Relaxed);
        info!("REPLICAOF NO ONE: promoted to primary — writes now accepted");
    }

    /// Called after every successful write: appends to AOF and fans out to replicas.
    async fn on_write(&self, resp: &str) {
        if let Some(aof) = &self.aof {
            aof.append(resp).await;
        }
        let bytes = resp.as_bytes().to_vec();
        let mut reg = self.replicas.lock().await;
        reg.retain(|tx| tx.send(bytes.clone()).is_ok());
    }

    /// Save snapshot then truncate AOF (snapshot subsumes the log).
    async fn save(&self, store: &KeyValueStore) {
        save_snapshot(store, &self.snap).await;
        if let Some(aof) = &self.aof {
            aof.truncate().await;
        }
    }
}

fn is_write_command(cmd: &Command) -> bool {
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
    )
}

// ── Replication server (primary side) ────────────────────────────────────────

async fn run_repl_server(
    port: u16,
    store: Arc<KeyValueStore>,
    snap_cfg: Arc<SnapshotConfig>,
    replicas: ReplRegistry,
    repl_password: Option<Arc<String>>,
) {
    let listener = match TcpListener::bind(format!("0.0.0.0:{}", port)).await {
        Ok(l) => l,
        Err(e) => {
            warn!("Replication listener failed to bind :{}: {}", port, e);
            return;
        }
    };
    info!("Replication server listening on 0.0.0.0:{}", port);
    loop {
        match listener.accept().await {
            Ok((socket, addr)) => {
                info!("Replica connected from {}", addr);
                let store = Arc::clone(&store);
                let snap_cfg = Arc::clone(&snap_cfg);
                let replicas = Arc::clone(&replicas);
                let pwd = repl_password.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_replica(socket, store, snap_cfg, replicas, pwd).await {
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
) -> std::io::Result<()> {
    // 0. Auth handshake — replica must send "<password>\n" before anything else
    if let Some(pwd) = &repl_password {
        let mut auth_buf = vec![0u8; pwd.len() + 1];
        socket.read_exact(&mut auth_buf).await?;
        let received_pwd = &auth_buf[..pwd.len()];
        let terminator = auth_buf[pwd.len()];
        if received_pwd != pwd.as_bytes() || terminator != b'\n' {
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
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    replicas.lock().await.push(tx);

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
                    sync_from_primary(&mut socket, &store, repl_password.as_deref()).await
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
        let mut cmd_bytes = vec![0u8; cmd_len];
        socket.read_exact(&mut cmd_bytes).await?;

        match Value::parse(&cmd_bytes) {
            Ok((value, _)) => {
                if let Ok(cmd) = Command::from_value(value) {
                    store.execute(cmd);
                }
            }
            Err(e) => warn!("Replica: bad command from primary: {}", e),
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
        }
    }

    fn punsubscribe(&mut self, conn_id: u64, pattern: &str) {
        self.pattern_subs
            .retain(|(p, id, _)| !(p == pattern && *id == conn_id));
    }

    fn unsubscribe_all(&mut self, conn_id: u64) {
        for v in self.channel_subs.values_mut() {
            v.retain(|(id, _)| *id != conn_id);
        }
        self.pattern_subs.retain(|(_, id, _)| *id != conn_id);
    }

    /// Deliver to all matching subscribers; returns the count delivered.
    fn publish(&mut self, channel: &str, message: &str) -> i64 {
        let mut count = 0i64;

        if let Some(subs) = self.channel_subs.get_mut(channel) {
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

type SharedPubSub = Arc<Mutex<PubSubHub>>;

// ── observable keys ───────────────────────────────────────────────────────────

type WatchNotif = (String, Value);
type WatchRegistry = Arc<Mutex<HashMap<String, Vec<(u64, mpsc::UnboundedSender<WatchNotif>)>>>>;

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
        | Command::ZIncrBy(k, _, _) => vec![k.clone()],
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

fn notify_watchers(
    registry: &WatchRegistry,
    cmd: &Command,
    response: &Value,
    store: &KeyValueStore,
) {
    if broadcast_for(cmd, response).is_none() {
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
    let mut reg = registry.lock().unwrap();
    for (key, value) in &key_values {
        if let Some(subs) = reg.get_mut(key) {
            subs.retain(|(_, tx)| tx.send((key.clone(), value.clone())).is_ok());
            if subs.is_empty() {
                reg.remove(key);
            }
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn encode_pubsub_msg(msg: PubSubMsg) -> Vec<u8> {
    match msg {
        PubSubMsg::Message { channel, message } => Value::Array(Some(vec![
            Value::BulkString(Some(b"message".to_vec())),
            Value::BulkString(Some(channel.into_bytes())),
            Value::BulkString(Some(message.into_bytes())),
        ]))
        .serialize(),
        PubSubMsg::PMessage {
            pattern,
            channel,
            message,
        } => Value::Array(Some(vec![
            Value::BulkString(Some(b"pmessage".to_vec())),
            Value::BulkString(Some(pattern.into_bytes())),
            Value::BulkString(Some(channel.into_bytes())),
            Value::BulkString(Some(message.into_bytes())),
        ]))
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

/// Encodes a list of string parts as a RESP bulk-string array.
fn resp_command(parts: &[&str]) -> String {
    let mut s = format!("*{}\r\n", parts.len());
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
                None => Some(resp_command(&["SET", k, v])),
                Some(SetExpiry::Ex(s)) => {
                    let px = s.saturating_mul(1000).to_string();
                    Some(resp_command(&["SET", k, v, "PX", &px]))
                }
                Some(SetExpiry::Px(ms)) => {
                    let ms_s = ms.to_string();
                    Some(resp_command(&["SET", k, v, "PX", &ms_s]))
                }
                Some(SetExpiry::Exat(ts)) => {
                    let pxat = ts.saturating_mul(1000).to_string();
                    Some(resp_command(&["SET", k, v, "PXAT", &pxat]))
                }
                Some(SetExpiry::Pxat(ts)) => {
                    let ts_s = ts.to_string();
                    Some(resp_command(&["SET", k, v, "PXAT", &ts_s]))
                }
                Some(SetExpiry::KeepTtl) => Some(resp_command(&["SET", k, v, "KEEPTTL"])),
            }
        }
        Command::Del(keys) | Command::Unlink(keys) => {
            let mut parts: Vec<&str> = vec!["DEL"];
            let key_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
            parts.extend_from_slice(&key_refs);
            Some(resp_command(&parts))
        }
        Command::MSet(pairs) => {
            let mut parts: Vec<&str> = vec!["MSET"];
            let flat: Vec<String> = pairs
                .iter()
                .flat_map(|(k, v)| [k.clone(), v.clone()])
                .collect();
            let flat_refs: Vec<&str> = flat.iter().map(|s| s.as_str()).collect();
            parts.extend_from_slice(&flat_refs);
            Some(resp_command(&parts))
        }
        Command::SetNx(k, v) => match response {
            Value::Integer(1) => Some(resp_command(&["SET", k, v])),
            _ => None,
        },
        Command::SetEx(k, secs, v) => {
            let px = secs.saturating_mul(1000).to_string();
            Some(resp_command(&["SET", k, v, "PX", &px]))
        }
        Command::PSetEx(k, ms, v) => {
            let ms_s = ms.to_string();
            Some(resp_command(&["SET", k, v, "PX", &ms_s]))
        }
        Command::Append(k, v) => match response {
            Value::Integer(_) => Some(resp_command(&["APPEND", k, v])),
            _ => None,
        },
        Command::GetSet(k, v) => Some(resp_command(&["SET", k, v])),
        Command::Incr(k) | Command::Decr(k) => match response {
            Value::Integer(n) => {
                let s = n.to_string();
                Some(resp_command(&["SET", k, &s]))
            }
            _ => None,
        },
        Command::IncrBy(k, _) | Command::DecrBy(k, _) => match response {
            Value::Integer(n) => {
                let s = n.to_string();
                Some(resp_command(&["SET", k, &s]))
            }
            _ => None,
        },
        Command::Expire(k, secs) => match response {
            Value::Integer(1) => {
                let ms = secs.saturating_mul(1000).to_string();
                Some(resp_command(&["PEXPIRE", k, &ms]))
            }
            _ => None,
        },
        Command::PExpire(k, ms) => match response {
            Value::Integer(1) => {
                let ms_s = ms.to_string();
                Some(resp_command(&["PEXPIRE", k, &ms_s]))
            }
            _ => None,
        },
        Command::ExpireAt(k, ts) => match response {
            Value::Integer(1) => {
                let ts_ms = ts.saturating_mul(1000).to_string();
                Some(resp_command(&["PEXPIREAT", k, &ts_ms]))
            }
            _ => None,
        },
        Command::PExpireAt(k, ts) => match response {
            Value::Integer(1) => {
                let ts_s = ts.to_string();
                Some(resp_command(&["PEXPIREAT", k, &ts_s]))
            }
            _ => None,
        },
        Command::Persist(k) => match response {
            Value::Integer(1) => Some(resp_command(&["PERSIST", k])),
            _ => None,
        },
        Command::FlushDb => Some(resp_command(&["FLUSHDB"])),
        Command::Rename(src, dst) => match response {
            Value::Error(_) => None,
            _ => Some(resp_command(&["RENAME", src, dst])),
        },

        // ── Hash ─────────────────────────────────────────────────────────────
        Command::HSet(k, pairs) => {
            let mut parts: Vec<String> = vec!["HSET".into(), k.clone()];
            for (f, v) in pairs {
                parts.push(f.clone());
                parts.push(v.clone());
            }
            let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
            Some(resp_command(&refs))
        }
        Command::HDel(k, fields) => match response {
            Value::Integer(n) if *n > 0 => {
                let mut parts: Vec<&str> = vec!["HDEL", k];
                let field_refs: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
                parts.extend_from_slice(&field_refs);
                Some(resp_command(&parts))
            }
            _ => None,
        },
        Command::HIncrBy(k, f, _) => match response {
            Value::Integer(n) => {
                let s = n.to_string();
                Some(resp_command(&["HSET", k, f, &s]))
            }
            _ => None,
        },
        Command::HIncrByFloat(k, f, _) => match response {
            Value::BulkString(Some(data)) => {
                let s = String::from_utf8_lossy(data);
                Some(resp_command(&["HSET", k, f, &s]))
            }
            _ => None,
        },
        Command::HSetNx(k, f, v) => match response {
            Value::Integer(1) => Some(resp_command(&["HSET", k, f, v])),
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
            Some(resp_command(&parts))
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
                Some(resp_command(&parts))
            }
            _ => None,
        },
        Command::LPop(k, count) => match response {
            Value::BulkString(None) => None,
            Value::Array(Some(items)) if items.is_empty() => None,
            _ => {
                let n = count.map(|c| c.to_string());
                match &n {
                    Some(ns) => Some(resp_command(&["LPOP", k, ns])),
                    None => Some(resp_command(&["LPOP", k])),
                }
            }
        },
        Command::RPop(k, count) => match response {
            Value::BulkString(None) => None,
            Value::Array(Some(items)) if items.is_empty() => None,
            _ => {
                let n = count.map(|c| c.to_string());
                match &n {
                    Some(ns) => Some(resp_command(&["RPOP", k, ns])),
                    None => Some(resp_command(&["RPOP", k])),
                }
            }
        },
        Command::LSet(k, idx, v) => match response {
            Value::SimpleString(_) => {
                let idx_s = idx.to_string();
                Some(resp_command(&["LSET", k, &idx_s, v]))
            }
            _ => None,
        },
        Command::LRem(k, count, elem) => match response {
            Value::Integer(n) if *n > 0 => {
                let count_s = count.to_string();
                Some(resp_command(&["LREM", k, &count_s, elem]))
            }
            _ => None,
        },
        Command::LTrim(k, start, stop) => {
            let start_s = start.to_string();
            let stop_s = stop.to_string();
            Some(resp_command(&["LTRIM", k, &start_s, &stop_s]))
        }

        // ── Set ───────────────────────────────────────────────────────────────
        Command::SAdd(k, members) => match response {
            Value::Integer(n) if *n > 0 => {
                let mut parts: Vec<&str> = vec!["SADD", k];
                let m_refs: Vec<&str> = members.iter().map(|s| s.as_str()).collect();
                parts.extend_from_slice(&m_refs);
                Some(resp_command(&parts))
            }
            _ => None,
        },
        Command::SRem(k, members) => match response {
            Value::Integer(n) if *n > 0 => {
                let mut parts: Vec<&str> = vec!["SREM", k];
                let m_refs: Vec<&str> = members.iter().map(|s| s.as_str()).collect();
                parts.extend_from_slice(&m_refs);
                Some(resp_command(&parts))
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
                Some(resp_command(&parts))
            }
        }
        Command::SMove(src, dst, member) => match response {
            Value::Integer(1) => Some(resp_command(&["SMOVE", src, dst, member])),
            _ => None,
        },
        Command::SInterStore(dst, keys) => {
            let mut parts: Vec<&str> = vec!["SINTERSTORE", dst];
            let k_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
            parts.extend_from_slice(&k_refs);
            Some(resp_command(&parts))
        }
        Command::SUnionStore(dst, keys) => {
            let mut parts: Vec<&str> = vec!["SUNIONSTORE", dst];
            let k_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
            parts.extend_from_slice(&k_refs);
            Some(resp_command(&parts))
        }
        Command::SDiffStore(dst, keys) => {
            let mut parts: Vec<&str> = vec!["SDIFFSTORE", dst];
            let k_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
            parts.extend_from_slice(&k_refs);
            Some(resp_command(&parts))
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
            Some(resp_command(&refs))
        }
        Command::ZRem(k, members) => match response {
            Value::Integer(n) if *n > 0 => {
                let mut parts: Vec<&str> = vec!["ZREM", k];
                let m_refs: Vec<&str> = members.iter().map(|s| s.as_str()).collect();
                parts.extend_from_slice(&m_refs);
                Some(resp_command(&parts))
            }
            _ => None,
        },
        Command::ZIncrBy(k, delta, member) => {
            let delta_s = format_f64_score(*delta);
            Some(resp_command(&["ZINCRBY", k, &delta_s, member]))
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
        Some(pwd) if provided == pwd => {
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // ── Prometheus metrics ────────────────────────────────────────────────
    let metrics_port: u16 = std::env::var("RECACHED_METRICS_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9091);
    let metrics_addr: std::net::SocketAddr = format!("0.0.0.0:{}", metrics_port).parse().unwrap();
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
    let save_interval_secs: u64 = std::env::var("RECACHED_SAVE_INTERVAL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(900);

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
    let replicas: ReplRegistry = Arc::new(tokio::sync::Mutex::new(Vec::new()));

    // ── server state ──────────────────────────────────────────────────────
    let state = Arc::new(ServerState {
        snap: Arc::clone(&snap_cfg),
        aof,
        replicas: Arc::clone(&replicas),
        is_replica: std::sync::atomic::AtomicBool::new(is_replica_start),
    });

    // ── autosave ──────────────────────────────────────────────────────────
    if save_interval_secs > 0 {
        let store_snap = Arc::clone(&store);
        let state_snap = Arc::clone(&state);
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(save_interval_secs));
            interval.tick().await; // skip immediate first tick
            loop {
                interval.tick().await;
                state_snap.save(&store_snap).await;
            }
        });
        info!(
            "Autosave every {}s → {:?}",
            save_interval_secs, snap_cfg.path
        );
    } else {
        info!("Autosave disabled (RECACHED_SAVE_INTERVAL=0). Use SAVE or BGSAVE manually.");
    }

    // ── start replication ─────────────────────────────────────────────────
    if !is_replica_start {
        let store_r = Arc::clone(&store);
        let snap_r = Arc::clone(&snap_cfg);
        let reg_r = Arc::clone(&replicas);
        let pwd_r = repl_password.clone().map(Arc::new);
        tokio::spawn(async move {
            run_repl_server(repl_port, store_r, snap_r, reg_r, pwd_r).await;
        });
    } else if let Some(primary_addr) = replicaof {
        let store_r = Arc::clone(&store);
        let state_r = Arc::clone(&state);
        let pwd_r = repl_password.clone();
        let fo_r = failover_timeout_secs;
        tokio::spawn(async move {
            run_repl_client(primary_addr, store_r, state_r, pwd_r, fo_r).await;
        });
        if let Some(t) = failover_timeout_secs {
            info!(
                "Running as replica — auto-failover enabled (promotes after {}s of primary being unreachable)",
                t
            );
        } else {
            info!("Running as replica — write commands will be rejected (auto-failover disabled; set RECACHED_FAILOVER_TIMEOUT to enable)");
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

    // ── broadcast channel (mutation sync) ────────────────────────────────
    // Carries (sender_conn_id, resp_encoded_mutation). WS receivers skip their own messages.
    let (tx, _rx) = broadcast::channel::<(u64, String)>(BROADCAST_CHANNEL_CAPACITY);

    // ── pub/sub hub ───────────────────────────────────────────────────────
    let pubsub: SharedPubSub = Arc::new(Mutex::new(PubSubHub::new()));

    // ── watch registry ────────────────────────────────────────────────────
    let watch_registry: WatchRegistry = Arc::new(Mutex::new(HashMap::new()));

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
    let tcp_listener = TcpListener::bind("0.0.0.0:6379").await?;
    info!("TCP server listening on 0.0.0.0:6379");

    let ws_listener = TcpListener::bind("0.0.0.0:6380").await?;
    info!("WebSocket server listening on 0.0.0.0:6380");

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
                                Ok(tls_stream) => handle_tcp(tls_stream, s, t, p, ps, wr, sc).await,
                                Err(e) => warn!("TCP TLS handshake failed from {}: {}", addr, e),
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
                        let id = next_conn_id();
                        tokio::spawn(async move {
                            let _permit = permit;
                            if let Some(acc) = tls.as_ref() {
                                match acc.accept(socket).await {
                                    Ok(tls_stream) => handle_ws(tls_stream, s, t, p, id, ps, wr, sc).await,
                                    Err(e) => warn!("WS TLS handshake failed from {}: {}", addr, e),
                                }
                            } else {
                                handle_ws(socket, s, t, p, id, ps, wr, sc).await;
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
    tx: broadcast::Sender<(u64, String)>,
    password: Arc<Option<String>>,
    pubsub: SharedPubSub,
    watch_registry: WatchRegistry,
    state: Arc<ServerState>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let _guard = ConnectionGuard::tcp();
    let (mut reader, mut writer) = tokio::io::split(socket);
    let mut buf = Vec::<u8>::new();
    let mut read_buf = [0u8; TCP_READ_BUFFER_BYTES];
    let mut is_authenticated = password.is_none();
    let mut auth_failures: u32 = 0;
    let mut multi_queue: Option<Vec<Command>> = None;
    let mut subscribed_channels: HashSet<String> = HashSet::new();
    let mut subscribed_patterns: HashSet<String> = HashSet::new();
    let (ps_tx, mut ps_rx) = mpsc::unbounded_channel::<PubSubMsg>();
    let conn_id = next_conn_id();

    'outer: loop {
        let is_subscribed = !subscribed_channels.is_empty() || !subscribed_patterns.is_empty();

        tokio::select! {
            result = reader.read(&mut read_buf) => {
                match result {
                    Ok(0) => break,
                    Ok(n) => {
                        if buf.len() + n > MAX_TCP_READ_BUFFER_BYTES {
                            warn!("TCP connection exceeded max buffer size, closing");
                            break 'outer;
                        }
                        buf.extend_from_slice(&read_buf[..n]);
                        'parse: loop {
                            match Value::parse(&buf) {
                                Ok((value, consumed)) => {
                                    buf.drain(..consumed);
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
                                        if disconnect { break 'outer; }
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
                                                    let mut results = Vec::with_capacity(queue.len());
                                                    for qcmd in queue {
                                                        let resp = execute_and_record(&store, &qcmd);
                                                        if let Some(msg) = broadcast_for(&qcmd, &resp) {
                                                            let _ = tx.send((0, msg.clone()));
                                                            state.on_write(&msg).await;
                                                        }
                                                        notify_watchers(&watch_registry, &qcmd, &resp, &store);
                                                        results.push(resp);
                                                    }
                                                    let out = Value::Array(Some(results)).serialize();
                                                    if writer.write_all(&out).await.is_err() { break 'outer; }
                                                }
                                            }
                                            continue 'parse;
                                        }
                                        _ => {}
                                    }

                                    // If inside MULTI, queue the command
                                    if let Some(ref mut queue) = multi_queue {
                                        // Pub/sub commands cannot be queued
                                        match &cmd {
                                            Command::Subscribe(_) | Command::Unsubscribe(_)
                                            | Command::PSubscribe(_) | Command::PUnsubscribe(_)
                                            | Command::Publish(_, _) => {
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
                                                pubsub.lock().unwrap().subscribe(conn_id, &ch, ps_tx.clone());
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
                                                pubsub.lock().unwrap().unsubscribe(conn_id, ch);
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
                                                pubsub.lock().unwrap().psubscribe(conn_id, &pat, ps_tx.clone());
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
                                                pubsub.lock().unwrap().punsubscribe(conn_id, pat);
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
                                            let count = pubsub.lock().unwrap().publish(&channel, &message);
                                            let resp = Value::Integer(count).serialize();
                                            if writer.write_all(&resp).await.is_err() { break 'outer; }
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
                                            let response = execute_and_record(&store, &cmd);
                                            if let Some(msg) = broadcast_for(&cmd, &response) {
                                                let _ = tx.send((0, msg.clone()));
                                                state.on_write(&msg).await;
                                            }
                                            notify_watchers(&watch_registry, &cmd, &response, &store);
                                            if writer.write_all(&response.serialize()).await.is_err() {
                                                break 'outer;
                                            }
                                        }
                                    }
                                }
                                Err(ref e) if e == "Incomplete" => break 'parse,
                                Err(e) => {
                                    warn!("TCP protocol error: {}", e);
                                    let _ = writer.write_all(b"-ERR Protocol error\r\n").await;
                                    buf.clear();
                                    break 'parse;
                                }
                            }
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
        }
    }

    if !subscribed_channels.is_empty() || !subscribed_patterns.is_empty() {
        pubsub.lock().unwrap().unsubscribe_all(conn_id);
    }
}

// ── WebSocket handler ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn handle_ws<S>(
    socket: S,
    store: Arc<KeyValueStore>,
    tx: broadcast::Sender<(u64, String)>,
    password: Arc<Option<String>>,
    conn_id: u64,
    pubsub: SharedPubSub,
    watch_registry: WatchRegistry,
    state: Arc<ServerState>,
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
    let (watch_tx, mut watch_rx) = mpsc::unbounded_channel::<WatchNotif>();

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
                                        let mut results = Vec::with_capacity(queue.len());
                                        for qcmd in queue {
                                            let resp = execute_and_record(&store, &qcmd);
                                            if let Some(msg) = broadcast_for(&qcmd, &resp) {
                                                let _ = tx.send((conn_id, msg.clone()));
                                                state.on_write(&msg).await;
                                            }
                                            notify_watchers(&watch_registry, &qcmd, &resp, &store);
                                            results.push(resp);
                                        }
                                        let out = Value::Array(Some(results)).serialize();
                                        ws_send!(&out);
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
                                | Command::Publish(_, _) => {
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
                                    pubsub.lock().unwrap().subscribe(conn_id, &ch, ps_tx.clone());
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
                                    pubsub.lock().unwrap().unsubscribe(conn_id, ch);
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
                                    pubsub.lock().unwrap().psubscribe(conn_id, &pat, ps_tx.clone());
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
                                    pubsub.lock().unwrap().punsubscribe(conn_id, pat);
                                    let count = subscribed_channels.len() + subscribed_patterns.len();
                                    ws_send!(&resp_subscribe_ack("punsubscribe", pat, count));
                                }
                                if targets.is_empty() {
                                    ws_send!(&resp_subscribe_ack("punsubscribe", "", 0));
                                }
                            }
                            Command::Publish(channel, message) => {
                                let count = pubsub.lock().unwrap().publish(&channel, &message);
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
                                        let mut reg = watch_registry.lock().unwrap();
                                        for key in &keys {
                                            if watched_keys.insert(key.clone()) {
                                                reg.entry(key.clone())
                                                    .or_default()
                                                    .push((conn_id, watch_tx.clone()));
                                            }
                                        }
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
                                    let mut reg = watch_registry.lock().unwrap();
                                    for key in &targets {
                                        if let Some(subs) = reg.get_mut(key) {
                                            subs.retain(|(id, _)| *id != conn_id);
                                            if subs.is_empty() {
                                                reg.remove(key);
                                            }
                                        }
                                    }
                                }
                                ws_send!(b"+OK\r\n");
                            }

                            cmd => {
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
                                let response = execute_and_record(&store, &cmd);
                                if let Some(b_msg) = broadcast_for(&cmd, &response) {
                                    if let Err(e) = tx.send((conn_id, b_msg.clone())) {
                                        debug!("WS broadcast on conn {} had no receivers: {}", conn_id, e);
                                    }
                                    state.on_write(&b_msg).await;
                                }
                                notify_watchers(&watch_registry, &cmd, &response, &store);
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
                    Ok((sender_id, msg)) if sender_id != conn_id => {
                        if ws_sender.send(Message::Text(msg.into())).await.is_err() {
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
        pubsub.lock().unwrap().unsubscribe_all(conn_id);
    }
    if !watched_keys.is_empty() {
        let mut reg = watch_registry.lock().unwrap();
        for key in &watched_keys {
            if let Some(subs) = reg.get_mut(key) {
                subs.retain(|(id, _)| *id != conn_id);
                if subs.is_empty() {
                    reg.remove(key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_engine::cmd::{SetOptions, ZAddOptions};
    use core_engine::resp::Value;
    use core_engine::store::KeyValueStore;
    use std::sync::atomic::AtomicI64;

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("recached_test_{name}_{}", std::process::id()))
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
}
