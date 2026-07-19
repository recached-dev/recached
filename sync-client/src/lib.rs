//! Platform-neutral Recached sync client.
//!
//! This crate is the *brain* of every Recached client SDK: the durable
//! outbox, exactly-once `DEDUP` envelopes, ordered-reply acknowledgment
//! correlation, session re-establishment after reconnect, and backoff
//! policy. It performs no I/O and knows nothing about WebSockets, IndexedDB,
//! or timers — methods take the current connection state and return
//! *effects* (frames to send, outbox rows to persist or delete, events to
//! surface) for a platform adapter to execute.
//!
//! Adapters: `wasm-edge` (browser: WebSocket + IndexedDB + `setTimeout`);
//! planned uniffi bindings for Kotlin/Swift and `flutter_rust_bridge`
//! (roadmap #6) reuse this crate unchanged, so merge semantics can never
//! drift between platforms.
//!
//! The wire protocol this implements is specified in `docs/server/protocol.md`.

use core_engine::cmd::Command;
use core_engine::resp::Value;
use core_engine::store::KeyValueStore;
use std::collections::VecDeque;
use std::sync::Arc;

/// Cap on outbox entries. When full, the oldest is dropped (reported via
/// [`Enqueued::dropped`] so durable storage can be cleaned up too).
pub const MAX_PENDING_WRITES: usize = 10_000;

/// Reconnect backoff: `500ms * 2^attempts`, capped.
pub const BACKOFF_BASE_MS: u32 = 500;
pub const BACKOFF_CAP_MS: u32 = 30_000;

/// FNV-1a over the client id — a stable, non-zero seed per client.
fn seed_from(client_id: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in client_id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h | 1
}

/// What an incoming frame turned out to be, and what the adapter must do.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    /// A mutation (server push, `keychange`, or `qstate`) was applied to the
    /// local store — notify UI subscribers.
    Applied,
    /// A pub/sub message — deliver to channel subscribers.
    PubSub { channel: String, message: String },
    /// A command reply. When `retired` is set, that outbox row is now
    /// acknowledged — delete it from durable storage.
    Reply { retired: Option<u64> },
    /// A `qstate` frame: the reply to a QSUB (retire the row, if any) *and*
    /// state applied to the local store (notify UI subscribers).
    AppliedReply { retired: Option<u64> },
    /// Unparseable or irrelevant — nothing to do.
    Ignored,
}

/// Result of queueing a write.
#[derive(Debug, PartialEq)]
pub struct Enqueued {
    /// Outbox row id — persist the row under this key.
    pub id: u64,
    /// The wire frame (dedup-wrapped when requested). This exact string is
    /// stored in the outbox and replayed on reconnect.
    pub frame: String,
    /// Send `frame` now (the caller reported the socket open, and the write
    /// was recorded as inflight).
    pub send_now: bool,
    /// The outbox overflowed and this row id was evicted — delete it from
    /// durable storage.
    pub dropped: Option<u64>,
}

/// The platform-neutral sync state machine. One instance per connection
/// identity (i.e. per cache).
pub struct SyncClient {
    store: Arc<KeyValueStore>,
    /// Writes not yet acknowledged by the server, in send order.
    outbox: VecDeque<(u64, String)>,
    outbox_seq: u64,
    /// One entry per frame sent on the current socket: `Some(outbox id)` for
    /// data writes, `None` for session commands. Replies arrive in send
    /// order; each acknowledges the front entry.
    inflight: VecDeque<Option<u64>>,
    client_id: String,
    /// Upper 32 bits of every dedup wire id; bump per session (persisted by
    /// the adapter) so a new session's ids clear the server's high-water mark.
    epoch: u32,
    password: Option<String>,
    sync_token: Option<String>,
    sync_scopes_csv: Option<String>,
    live_queries: Vec<String>,
    attempts: u32,
    /// Jitter source for reconnect backoff. Seeded from `client_id` rather than
    /// a system RNG so this crate stays dependency-free and I/O-free — and so
    /// the sequence is reproducible in tests. Different clients get different
    /// sequences, which is the only property that matters here.
    jitter: u64,
}

impl SyncClient {
    pub fn new(store: Arc<KeyValueStore>, client_id: String) -> Self {
        let jitter = seed_from(&client_id);
        Self {
            store,
            outbox: VecDeque::new(),
            outbox_seq: 0,
            inflight: VecDeque::new(),
            client_id,
            epoch: 0,
            password: None,
            sync_token: None,
            sync_scopes_csv: None,
            live_queries: Vec::new(),
            attempts: 0,
            jitter,
        }
    }

    pub fn store(&self) -> &Arc<KeyValueStore> {
        &self.store
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn set_epoch(&mut self, epoch: u32) {
        self.epoch = epoch;
    }

    /// True while nothing has been written this session — the window in which
    /// adopting a persisted client identity is safe.
    pub fn no_writes_yet(&self) -> bool {
        self.outbox_seq == 0 && self.outbox.is_empty()
    }

    /// Adopt a persisted client id. Refused (returns false) once writes have
    /// been issued — switching identity mid-stream would fragment the
    /// server's high-water mark.
    pub fn adopt_client_id(&mut self, client_id: String) -> bool {
        if self.no_writes_yet() {
            self.client_id = client_id;
            true
        } else {
            false
        }
    }

    // ── session state ─────────────────────────────────────────────────────
    // Setters record state for re-establishment on every reconnect and, when
    // the socket is open, return the frame to send now (recording it as
    // inflight — session replies must occupy a reply slot or they would
    // falsely acknowledge a data write).

    pub fn set_password(&mut self, password: &str, connected: bool) -> Option<String> {
        self.password = Some(password.to_string());
        self.session_frame(to_resp(&["AUTH", password]), connected)
    }

    pub fn set_sync_token(&mut self, token: &str, connected: bool) -> Option<String> {
        self.sync_token = Some(token.to_string());
        self.session_frame(to_resp(&["SYNC", "TOKEN", token]), connected)
    }

    /// Comma-separated glob patterns. Returns `None` (and records nothing)
    /// when the list is empty.
    pub fn set_sync_scopes(&mut self, patterns_csv: &str, connected: bool) -> Option<String> {
        let frame = sync_scopes_frame(patterns_csv)?;
        self.sync_scopes_csv = Some(patterns_csv.to_string());
        self.session_frame(frame, connected)
    }

    /// Register a live query (idempotent). The returned frame re-hydrates
    /// matching keys via the `qstate` reply.
    pub fn add_live_query(&mut self, pattern: &str, connected: bool) -> Option<String> {
        if !self.live_queries.iter().any(|p| p == pattern) {
            self.live_queries.push(pattern.to_string());
        }
        self.session_frame(to_resp(&["QSUB", pattern]), connected)
    }

    pub fn remove_live_query(&mut self, pattern: Option<&str>, connected: bool) -> Option<String> {
        let frame = match pattern {
            Some(p) => {
                self.live_queries.retain(|q| q != p);
                to_resp(&["QUNSUB", p])
            }
            None => {
                self.live_queries.clear();
                to_resp(&["QUNSUB"])
            }
        };
        self.session_frame(frame, connected)
    }

    fn session_frame(&mut self, frame: String, connected: bool) -> Option<String> {
        if connected {
            self.inflight.push_back(None);
            Some(frame)
        } else {
            None
        }
    }

    // ── connection lifecycle ──────────────────────────────────────────────

    /// The socket opened: returns every frame to send, in order — session
    /// state first (AUTH → SYNC → live queries), then the full outbox replay.
    /// Live-query re-subscription re-hydrates local state; outbox entries
    /// stay queued until their replies acknowledge them.
    pub fn on_open(&mut self) -> Vec<String> {
        self.attempts = 0;
        self.inflight.clear();
        let mut frames = Vec::new();
        if let Some(pwd) = &self.password {
            frames.push(to_resp(&["AUTH", pwd]));
            self.inflight.push_back(None);
        }
        if let Some(tok) = &self.sync_token {
            frames.push(to_resp(&["SYNC", "TOKEN", tok]));
            self.inflight.push_back(None);
        } else if let Some(csv) = &self.sync_scopes_csv
            && let Some(frame) = sync_scopes_frame(csv)
        {
            frames.push(frame);
            self.inflight.push_back(None);
        }
        for pat in &self.live_queries {
            frames.push(to_resp(&["QSUB", pat]));
            self.inflight.push_back(None);
        }
        for (id, frame) in &self.outbox {
            frames.push(frame.clone());
            self.inflight.push_back(Some(*id));
        }
        frames
    }

    /// The socket closed: returns the delay before the next reconnect
    /// attempt (exponential backoff, capped).
    /// Delay before the next reconnect attempt, in milliseconds.
    ///
    /// Exponential with a cap, then **jittered into `[delay/2, delay]`**. Without
    /// jitter every client disconnected by the same event computes an identical
    /// schedule and reconnects in lockstep — a thundering herd that can keep a
    /// recovering server down. Halving the floor keeps the growth shape intact
    /// while spreading arrivals across the window.
    pub fn on_close(&mut self) -> u32 {
        let delay = BACKOFF_BASE_MS
            .saturating_mul(1u32 << self.attempts.min(6))
            .min(BACKOFF_CAP_MS);
        self.attempts = self.attempts.saturating_add(1);

        let half = delay / 2;
        if half == 0 {
            return delay;
        }
        half + (self.next_jitter() % (half as u64 + 1)) as u32
    }

    /// xorshift64* — small, dependency-free, and good enough for spreading
    /// reconnects. Not for anything security-sensitive.
    fn next_jitter(&mut self) -> u64 {
        let mut x = self.jitter;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.jitter = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    // ── writes ────────────────────────────────────────────────────────────

    /// Queue a write for the server. `dedup` wraps it in an exactly-once
    /// envelope — store writes want this; connection-scoped commands
    /// (pub/sub) must not be wrapped, or a legitimately replayed SUBSCRIBE
    /// would be skipped as a duplicate.
    pub fn enqueue_write(&mut self, encoded: &str, dedup: bool, connected: bool) -> Enqueued {
        let id = self.outbox_seq;
        self.outbox_seq += 1;
        let frame = if dedup {
            self.wrap_dedup(encoded, id)
        } else {
            encoded.to_string()
        };
        let dropped = if self.outbox.len() >= MAX_PENDING_WRITES {
            self.outbox.pop_front().map(|(old, _)| old)
        } else {
            None
        };
        self.outbox.push_back((id, frame.clone()));
        if connected {
            self.inflight.push_back(Some(id));
        }
        Enqueued {
            id,
            frame,
            send_now: connected,
            dropped,
        }
    }

    /// Splice a `DEDUP <client> <wire-id>` envelope into an already-encoded
    /// RESP array: bump the element count and prepend three elements.
    fn wrap_dedup(&self, plain: &str, outbox_id: u64) -> String {
        let Some((head, rest)) = plain.split_once("\r\n") else {
            return plain.to_string();
        };
        let n: usize = head.trim_start_matches('*').parse().unwrap_or(0);
        if n == 0 {
            return plain.to_string();
        }
        let wire_id = ((self.epoch as u64) << 32) | (outbox_id & 0xFFFF_FFFF);
        let id_s = wire_id.to_string();
        format!(
            "*{}\r\n$5\r\nDEDUP\r\n${}\r\n{}\r\n${}\r\n{}\r\n{}",
            n + 3,
            self.client_id.len(),
            self.client_id,
            id_s.len(),
            id_s,
            rest
        )
    }

    /// Restore outbox rows persisted by a previous session (ordered by their
    /// stored key). Rows are renumbered with fresh ids so they can never
    /// collide with ids handed out this session; the wire frames are stored
    /// verbatim, so their embedded dedup ids still match what the server may
    /// have already applied. Returns `(old_id, new_id, frame)` for the
    /// adapter to rewrite durable storage.
    pub fn restore_outbox(&mut self, mut rows: Vec<(u64, String)>) -> Vec<(u64, u64, String)> {
        rows.sort_by_key(|(id, _)| *id);
        let mut rewrites = Vec::with_capacity(rows.len());
        for (old_id, frame) in rows.into_iter().rev() {
            let new_id = self.outbox_seq;
            self.outbox_seq += 1;
            self.outbox.push_front((new_id, frame.clone()));
            rewrites.push((old_id, new_id, frame));
        }
        rewrites
    }

    /// Discard all queued writes and reply bookkeeping (sign-out semantics).
    pub fn clear_outbox(&mut self) {
        self.outbox.clear();
        self.inflight.clear();
    }

    pub fn outbox_len(&self) -> usize {
        self.outbox.len()
    }

    // ── incoming frames ───────────────────────────────────────────────────

    /// Classify and apply one incoming text frame.
    ///
    /// Pushes (RESP3 Push mutations, pub/sub, `keychange` arrays) are applied
    /// or surfaced; everything else is a command reply that acknowledges the
    /// oldest inflight command — `qstate` arrays are both the reply to a
    /// QSUB *and* state to apply. See `docs/server/protocol.md`.
    pub fn handle_frame(&mut self, text: &str) -> Incoming {
        match Value::parse(text.as_bytes()) {
            Ok((Value::Push(arr), _)) => {
                if arr.len() == 3
                    && let (
                        Value::BulkString(Some(kind)),
                        Value::BulkString(Some(channel)),
                        Value::BulkString(Some(payload)),
                    ) = (&arr[0], &arr[1], &arr[2])
                    && kind.eq_ignore_ascii_case(b"message")
                {
                    return Incoming::PubSub {
                        channel: String::from_utf8_lossy(channel).into_owned(),
                        message: String::from_utf8_lossy(payload).into_owned(),
                    };
                }
                if let Ok(cmd) = Command::from_value(Value::Array(Some(arr)))
                    && is_replayable_mutation(&cmd)
                {
                    self.store.execute(cmd);
                    return Incoming::Applied;
                }
                Incoming::Ignored
            }
            Ok((Value::Array(Some(items)), _)) => {
                let tag: &[u8] = match items.first() {
                    Some(Value::BulkString(Some(t))) => t,
                    _ => &[],
                };
                if tag == b"keychange" {
                    self.apply_keychange(&items);
                    Incoming::Applied
                } else if tag == b"qstate" {
                    let retired = self.ack_reply();
                    self.apply_qstate(&items);
                    Incoming::AppliedReply { retired }
                } else {
                    Incoming::Reply {
                        retired: self.ack_reply(),
                    }
                }
            }
            Ok(_) => Incoming::Reply {
                retired: self.ack_reply(),
            },
            Err(_) => Incoming::Ignored,
        }
    }

    /// A reply arrived: acknowledge the oldest inflight command. Returns the
    /// retired outbox row id for durable deletion, if it was a data write.
    fn ack_reply(&mut self) -> Option<u64> {
        let id = self.inflight.pop_front()??;
        self.outbox.retain(|(i, _)| *i != id);
        Some(id)
    }

    fn apply_keychange(&self, items: &[Value]) {
        if items.len() != 3 {
            return;
        }
        let Value::BulkString(Some(key)) = &items[1] else {
            return;
        };
        let key = String::from_utf8_lossy(key).into_owned();
        match &items[2] {
            Value::BulkString(Some(v)) => {
                self.store.execute(Command::Set(
                    key,
                    String::from_utf8_lossy(v).into_owned(),
                    Default::default(),
                ));
            }
            Value::BulkString(None) => {
                // A nil value whose "key" is one of our registered live-query
                // patterns is the FLUSHDB sentinel: every matching key is gone.
                // Sending one frame per deleted key would mean millions of
                // frames for a single command, so the server announces it once
                // per pattern and the client expands it locally.
                if self.live_queries.contains(&key) {
                    let matched: Vec<String> = self
                        .store
                        .matching_key_values(&key, usize::MAX)
                        .into_iter()
                        .map(|(k, _)| k)
                        .collect();
                    if !matched.is_empty() {
                        self.store.execute(Command::Del(matched));
                    }
                } else {
                    self.store.execute(Command::Del(vec![key]));
                }
            }
            // Type-tagged collection: ["hash"|"list"|"set"|"zset"|"json", ...].
            Value::Array(Some(items)) => self.apply_tagged(key, items),
            // Anything else (e.g. the "ratelimit" marker) carries no client-
            // usable value.
            _ => {}
        }
    }

    /// Apply a type-tagged collection value from a keychange or qstate frame.
    ///
    /// The key is replaced wholesale rather than merged: the frame carries the
    /// complete current value, so rebuilding is both simpler and correct when a
    /// member was removed. Anything unrecognised is ignored rather than guessed
    /// at, so a newer server cannot corrupt an older client's local copy.
    fn apply_tagged(&self, key: String, items: &[Value]) {
        fn text(v: &Value) -> Option<String> {
            match v {
                Value::BulkString(Some(b)) => Some(String::from_utf8_lossy(b).into_owned()),
                Value::SimpleString(s) => Some(s.clone()),
                _ => None,
            }
        }
        let Some(tag) = items.first().and_then(text) else {
            return;
        };
        let rest: Vec<String> = items[1..].iter().filter_map(text).collect();

        // Clear first so removed members do not linger.
        self.store.execute(Command::Del(vec![key.clone()]));
        match tag.as_str() {
            "hash" => {
                let pairs: Vec<(String, String)> = rest
                    .chunks_exact(2)
                    .map(|c| (c[0].clone(), c[1].clone()))
                    .collect();
                if !pairs.is_empty() {
                    self.store.execute(Command::HSet(key, pairs));
                }
            }
            "list" => {
                if !rest.is_empty() {
                    self.store.execute(Command::RPush(key, rest));
                }
            }
            "set" => {
                if !rest.is_empty() {
                    self.store.execute(Command::SAdd(key, rest));
                }
            }
            "zset" => {
                let members: Vec<(f64, String)> = rest
                    .chunks_exact(2)
                    .filter_map(|c| c[1].parse::<f64>().ok().map(|sc| (sc, c[0].clone())))
                    .collect();
                if !members.is_empty() {
                    self.store
                        .execute(Command::ZAdd(key, Default::default(), members));
                }
            }
            "json" => {
                if let Some(doc) = rest.first() {
                    self.store
                        .execute(Command::JSet(key, "$".to_string(), doc.clone()));
                }
            }
            _ => {}
        }
    }

    fn apply_qstate(&self, items: &[Value]) {
        for pair in items.get(2..).unwrap_or(&[]).chunks_exact(2) {
            let Value::BulkString(Some(k)) = &pair[0] else {
                continue;
            };
            let key = String::from_utf8_lossy(k).into_owned();
            match &pair[1] {
                Value::BulkString(Some(v)) => {
                    self.store.execute(Command::Set(
                        key,
                        String::from_utf8_lossy(v).into_owned(),
                        Default::default(),
                    ));
                }
                // Collections arrive type-tagged, so the initial state of a
                // live query is complete — no follow-up typed read needed.
                Value::Array(Some(inner)) => self.apply_tagged(key, inner),
                _ => {}
            }
        }
    }
}

fn sync_scopes_frame(patterns_csv: &str) -> Option<String> {
    let pats: Vec<&str> = patterns_csv
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if pats.is_empty() {
        return None;
    }
    let mut parts = vec!["SYNC"];
    parts.extend(pats);
    Some(to_resp(&parts))
}

/// Mutations another peer may push at us that are safe to replay into the
/// local store. Everything else (command replies, admin, unknown) is ignored.
pub fn is_replayable_mutation(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Set(_, _, _)
            | Command::Del(_)
            | Command::Unlink(_)
            | Command::MSet(_)
            | Command::Incr(_)
            | Command::Decr(_)
            | Command::IncrBy(_, _)
            | Command::DecrBy(_, _)
            | Command::Expire(_, _)
            | Command::PExpire(_, _)
            | Command::ExpireAt(_, _)
            | Command::PExpireAt(_, _)
            | Command::Persist(_)
            | Command::FlushDb
            | Command::Rename(_, _)
            | Command::HSet(_, _)
            | Command::HDel(_, _)
            | Command::HSetNx(_, _, _)
            | Command::LPush(_, _)
            | Command::RPush(_, _)
            | Command::LPop(_, _)
            | Command::RPop(_, _)
            | Command::LSet(_, _, _)
            | Command::LRem(_, _, _)
            | Command::LTrim(_, _, _)
            | Command::SAdd(_, _)
            | Command::SRem(_, _)
            | Command::SMove(_, _, _)
            | Command::SInterStore(_, _)
            | Command::SUnionStore(_, _)
            | Command::SDiffStore(_, _)
            | Command::ZAdd(_, _, _)
            | Command::ZRem(_, _)
            | Command::ZIncrBy(_, _, _)
            | Command::JSet(_, _, _)
            | Command::JMerge(_, _)
    )
}

/// RESP array encoding, shared by adapters.
pub fn to_resp(parts: &[&str]) -> String {
    let mut s = format!("*{}\r\n", parts.len());
    for part in parts {
        s.push_str(&format!("${}\r\n{}\r\n", part.len(), part));
    }
    s
}

#[cfg(test)]
mod tests;
