//! Hold a live slice of a Recached cache in your own process.
//!
//! `recached-edge` lets a browser keep a local copy of the cache and receive
//! pushes when it changes. This crate is the same thing for a Rust service:
//! reads come from local memory, and the WebSocket carries only writes out and
//! change notices in.
//!
//! ```no_run
//! # async fn demo() -> Result<(), recached_embed::Error> {
//! use recached_embed::Cache;
//!
//! let cache = Cache::connect("ws://127.0.0.1:6380").await?;
//!
//! // Declare the working set once, at start-up. Returns after the initial
//! // state has landed, so the first request never races hydration.
//! cache.watch("fare:*").await?;
//!
//! // Hot path: no network, no await, no lock.
//! let fare = cache.get("fare:MNL-CEB")?;
//! # Ok(())
//! # }
//! ```
//!
//! # What this is for
//!
//! Small, hot, shared, read-heavy, **slowly-changing** data: fare tables,
//! feature flags, tenant settings, entitlement checks, rate-limit tiers. The
//! win is that a read costs a pointer chase instead of a round-trip, while the
//! server still tells every instance the moment something changes.
//!
//! It is the wrong tool for data that changes constantly (live counters, seat
//! inventory), working sets that do not fit in each process, keys read once, or
//! anything where being stale by milliseconds is unacceptable. Use the server
//! directly for those.
//!
//! # Hydration is part of the contract
//!
//! The local store holds only what your [`watch`](Cache::watch) patterns
//! hydrated. Reading anything else returns [`Error::NotHydrated`] rather than
//! `None`, because a silent `None` is indistinguishable from "the key does not
//! exist" — which is how an embedded cache serves confidently wrong answers.
//! [`get_or_fetch`](Cache::get_or_fetch) is the escape hatch when you would
//! rather pay for a round-trip than declare a pattern.

mod conn;
mod error;

pub use core_engine::resp::Value;
pub use error::{Error, Result};

use conn::{Ack, Op, Shared, Task};
use core_engine::store::{KeyValueStore, glob_match};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use sync_client::{SyncClient, to_resp, to_resp_bytes};
use tokio::sync::{broadcast, mpsc, oneshot};

/// A handle to the local cache. Cheap to clone and safe to share across tasks;
/// every clone reads the same store.
#[derive(Clone)]
pub struct Cache {
    inner: Arc<Inner>,
}

struct Inner {
    /// The local copy. `execute` takes `&self`, so reads need no outer lock.
    store: Arc<KeyValueStore>,
    ops: mpsc::UnboundedSender<Op>,
    /// Live-query patterns whose initial state has landed.
    hydrated: RwLock<Vec<String>>,
    shared: Arc<Shared>,
}

/// Configuration for [`Cache::builder`].
pub struct CacheBuilder {
    url: String,
    password: Option<String>,
    sync_token: Option<String>,
    max_pending: Option<usize>,
}

impl CacheBuilder {
    /// `RECACHED_PASSWORD` for the server.
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// A scoped sync token (see `docs/server/sync-scopes.md`). Required when
    /// the server runs with `RECACHED_SYNC_SECRET` set.
    pub fn sync_token(mut self, token: impl Into<String>) -> Self {
        self.sync_token = Some(token.into());
        self
    }

    /// Cap on writes queued while disconnected. Beyond it the oldest is
    /// dropped. The default suits a client that is briefly offline; a service
    /// taking heavy write traffic through a long partition should size this
    /// deliberately.
    pub fn max_pending(mut self, max: usize) -> Self {
        self.max_pending = Some(max);
        self
    }

    /// Connect, returning once the socket is open. A later drop reconnects in
    /// the background with jittered backoff.
    pub async fn connect(self) -> Result<Cache> {
        let store = Arc::new(KeyValueStore::new());
        let client_id = format!("{:032x}", rand::random::<u128>());
        let mut client = SyncClient::new(Arc::clone(&store), client_id);

        if let Some(max) = self.max_pending {
            client.set_max_pending(max);
        }
        // Recorded, not sent: `on_open` emits these in the right order on this
        // connection and on every reconnect.
        if let Some(password) = &self.password {
            client.set_password(password, false);
        }
        if let Some(token) = &self.sync_token {
            client.set_sync_token(token, false);
        }

        let shared = Arc::new(Shared {
            connected: AtomicBool::new(false),
            outbox_len: AtomicUsize::new(0),
        });
        let (ops_tx, ops_rx) = mpsc::unbounded_channel();
        let (open_tx, open_rx) = oneshot::channel();

        let task = Task::new(client, Arc::clone(&shared), self.url, open_tx);
        tokio::spawn(task.run(ops_rx));

        open_rx.await.map_err(|_| Error::Closed)??;

        Ok(Cache {
            inner: Arc::new(Inner {
                store,
                ops: ops_tx,
                hydrated: RwLock::new(Vec::new()),
                shared,
            }),
        })
    }
}

impl Cache {
    /// Start configuring a connection to `url` (`ws://host:6380`).
    ///
    /// Note the port: live queries and change pushes travel on the WebSocket
    /// sync path, not RESP on 6379. A plain TCP client is never sent keychange
    /// notifications, so it cannot hold a coherent local copy.
    pub fn builder(url: impl Into<String>) -> CacheBuilder {
        CacheBuilder {
            url: url.into(),
            password: None,
            sync_token: None,
            max_pending: None,
        }
    }

    /// Connect with defaults.
    pub async fn connect(url: impl Into<String>) -> Result<Cache> {
        Self::builder(url).connect().await
    }

    // ── working set ───────────────────────────────────────────────────────

    /// Hydrate every key matching `pattern` and keep it current.
    ///
    /// Returns only after the server's initial state has been applied, so this
    /// doubles as a start-up barrier. The server caps initial state at 10,000
    /// keys per pattern; a pattern matching more is a sign the working set is
    /// too big to embed.
    ///
    /// Re-subscription after a reconnect is automatic.
    pub async fn watch(&self, pattern: impl Into<String>) -> Result<()> {
        let pattern = pattern.into();
        self.round_trip(|ack| Op::Watch {
            pattern: pattern.clone(),
            ack,
        })
        .await?;

        let mut hydrated = self
            .inner
            .hydrated
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if !hydrated.contains(&pattern) {
            hydrated.push(pattern);
        }
        Ok(())
    }

    /// Stop tracking `pattern`. Keys it hydrated stay in local memory but stop
    /// receiving updates, so they are dropped from the hydrated set — reading
    /// them now returns [`Error::NotHydrated`] rather than a stale value.
    pub async fn unwatch(&self, pattern: impl Into<String>) -> Result<()> {
        let pattern = pattern.into();
        self.round_trip(|ack| Op::Unwatch {
            pattern: pattern.clone(),
            ack,
        })
        .await?;

        let mut hydrated = self
            .inner
            .hydrated
            .write()
            .unwrap_or_else(|e| e.into_inner());
        hydrated.retain(|p| p != &pattern);
        Ok(())
    }

    /// Whether `key` is covered by a live query, i.e. whether [`get`](Self::get)
    /// can answer for it.
    pub fn is_hydrated(&self, key: &str) -> bool {
        let hydrated = self
            .inner
            .hydrated
            .read()
            .unwrap_or_else(|e| e.into_inner());
        hydrated.iter().any(|p| glob_match(p, key))
    }

    // ── local reads (no network) ──────────────────────────────────────────

    /// Read a string value from local memory.
    ///
    /// `Ok(None)` means the key genuinely does not exist. A key outside every
    /// watched pattern is [`Error::NotHydrated`], never `None`.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.ensure_hydrated(key)?;
        as_bytes(key, self.inner.store.get_current(key))
    }

    /// [`get`](Self::get) with UTF-8 decoding.
    pub fn get_str(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .get(key)?
            .map(|b| String::from_utf8_lossy(&b).into_owned()))
    }

    /// The raw local value, including collection types (returned as the
    /// type-tagged arrays described in `docs/server/protocol.md`).
    pub fn get_value(&self, key: &str) -> Result<Value> {
        self.ensure_hydrated(key)?;
        Ok(self.inner.store.get_current(key))
    }

    /// Whether the key exists locally.
    pub fn contains(&self, key: &str) -> Result<bool> {
        self.ensure_hydrated(key)?;
        Ok(!matches!(
            self.inner.store.get_current(key),
            Value::BulkString(None)
        ))
    }

    /// Every hydrated key matching `pattern`, from local memory.
    ///
    /// This scans the local keyspace, so it is O(n) in the number of cached
    /// keys — fine at start-up or on a background tick, not on a hot path.
    pub fn matching(&self, pattern: &str) -> Vec<(String, Value)> {
        self.inner.store.matching_key_values(pattern, usize::MAX)
    }

    // ── round-trips ───────────────────────────────────────────────────────

    /// Local read, falling back to one round-trip when the key is not covered
    /// by a watched pattern.
    ///
    /// The fetched value is **not** cached: nothing is keeping it current, and
    /// storing an unwatched key locally would make it stale on the first
    /// change. Prefer [`watch`](Self::watch) for anything read repeatedly.
    pub async fn get_or_fetch(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match self.get(key) {
            Err(Error::NotHydrated { .. }) => {}
            other => return other,
        }
        let reply = self
            .round_trip(|ack| Op::Read {
                frame: to_resp(&["GET", key]),
                ack,
            })
            .await?;
        as_bytes(key, reply)
    }

    // ── writes ────────────────────────────────────────────────────────────

    /// Set a key. Resolves when the server acknowledges the write.
    ///
    /// While disconnected the write is queued durably and replayed on
    /// reconnect, and this returns [`Error::Disconnected`] — the write is not
    /// lost, only unacknowledged.
    pub async fn set(&self, key: &str, value: impl AsRef<[u8]>) -> Result<()> {
        self.write(to_resp_bytes(&[b"SET", key.as_bytes(), value.as_ref()]))
            .await
            .map(drop)
    }

    /// Set a key with a time-to-live.
    pub async fn set_ex(&self, key: &str, value: impl AsRef<[u8]>, ttl: Duration) -> Result<()> {
        let ms = ttl.as_millis().to_string();
        self.write(to_resp_bytes(&[
            b"SET",
            key.as_bytes(),
            value.as_ref(),
            b"PX",
            ms.as_bytes(),
        ]))
        .await
        .map(drop)
    }

    /// Delete a key.
    pub async fn del(&self, key: &str) -> Result<()> {
        self.write(to_resp(&["DEL", key])).await.map(drop)
    }

    /// Add to a counter, returning its new value.
    pub async fn incr_by(&self, key: &str, delta: i64) -> Result<i64> {
        let reply = self
            .write(to_resp(&["INCRBY", key, &delta.to_string()]))
            .await?;
        match reply {
            Value::Integer(n) => Ok(n),
            _ => Err(Error::WrongType {
                key: key.to_string(),
            }),
        }
    }

    /// Send an arbitrary mutation, returning the server's reply.
    ///
    /// Goes through the outbox: it is replayed on reconnect and wrapped in an
    /// exactly-once envelope, exactly like [`set`](Self::set). Use it for
    /// commands the typed helpers do not cover (`HSET`, `SADD`, `ZADD`, …).
    ///
    /// Only pass mutations. A read sent here would be replayed pointlessly
    /// after a reconnect — use [`read_command`](Self::read_command) instead.
    pub async fn write_command(&self, args: &[&str]) -> Result<Value> {
        self.write(to_resp(args)).await
    }

    /// Send an arbitrary command as a one-shot round-trip, never replayed.
    ///
    /// The reply is returned as-is and nothing is applied to the local store.
    pub async fn read_command(&self, args: &[&str]) -> Result<Value> {
        let frame = to_resp(args);
        self.round_trip(|ack| Op::Read { frame, ack }).await
    }

    // ── pub/sub ───────────────────────────────────────────────────────────

    /// Subscribe to a channel. Re-subscribed automatically after a reconnect;
    /// messages published during the gap are lost.
    pub async fn subscribe(
        &self,
        channel: impl Into<String>,
    ) -> Result<broadcast::Receiver<Vec<u8>>> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .ops
            .send(Op::Subscribe {
                channel: channel.into(),
                ack: tx,
            })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    /// Publish to a channel.
    ///
    /// Sent one-shot rather than through the outbox: replaying a publish after
    /// a reconnect would deliver it twice.
    pub async fn publish(&self, channel: &str, message: impl AsRef<[u8]>) -> Result<()> {
        self.round_trip(|ack| Op::Read {
            frame: to_resp_bytes(&[b"PUBLISH", channel.as_bytes(), message.as_ref()]),
            ack,
        })
        .await
        .map(drop)
    }

    // ── introspection ─────────────────────────────────────────────────────

    /// Whether the sync socket is currently up. Local reads keep working
    /// either way — they just stop receiving updates.
    pub fn is_connected(&self) -> bool {
        self.inner.shared.connected.load(Ordering::Acquire)
    }

    /// Writes queued but not yet acknowledged by the server.
    pub fn pending_writes(&self) -> usize {
        self.inner.shared.outbox_len.load(Ordering::Acquire)
    }

    /// Approximate bytes the local copy occupies in this process.
    pub fn local_bytes(&self) -> usize {
        self.inner.store.approximate_memory_bytes()
    }

    /// The underlying local store, for reads this API does not wrap.
    ///
    /// Bypasses the hydration check — anything read here may be absent simply
    /// because it was never watched.
    pub fn store(&self) -> &Arc<KeyValueStore> {
        &self.inner.store
    }

    // ── internals ─────────────────────────────────────────────────────────

    fn ensure_hydrated(&self, key: &str) -> Result<()> {
        if self.is_hydrated(key) {
            Ok(())
        } else {
            Err(Error::NotHydrated {
                key: key.to_string(),
            })
        }
    }

    async fn round_trip(&self, make: impl FnOnce(Ack) -> Op) -> Result<Value> {
        let (tx, rx) = oneshot::channel();
        self.inner.ops.send(make(tx)).map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }

    async fn write(&self, encoded: Vec<u8>) -> Result<Value> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .ops
            .send(Op::Write {
                encoded,
                ack: Some(tx),
            })
            .map_err(|_| Error::Closed)?;
        rx.await.map_err(|_| Error::Closed)?
    }
}

fn as_bytes(key: &str, value: Value) -> Result<Option<Vec<u8>>> {
    match value {
        Value::BulkString(Some(bytes)) => Ok(Some(bytes)),
        Value::BulkString(None) => Ok(None),
        Value::SimpleString(s) => Ok(Some(s.into_bytes())),
        Value::Integer(n) => Ok(Some(n.to_string().into_bytes())),
        _ => Err(Error::WrongType {
            key: key.to_string(),
        }),
    }
}
