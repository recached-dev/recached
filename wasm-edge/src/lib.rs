use core_engine::cmd::{Command, SetOptions};
use core_engine::resp::Value;
use core_engine::store::{KeyValueStore, SnapshotEntry, SnapshotValue, format_score};
use js_sys::Promise;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{BroadcastChannel, Event, MessageEvent, WebSocket};

// ── IndexedDB JS glue ─────────────────────────────────────────────────────────
//
// WAL schema: object store "wal", out-of-line keys (seq number as f64),
// values are RESP-encoded command strings. IDB returns entries in ascending
// key order, so keys[i] and vals[i] always correspond after idbReadAll.

#[wasm_bindgen(inline_js = r#"
export function openRecachedDb() {
    return new Promise((resolve, reject) => {
        const req = indexedDB.open('recached', 1);
        req.onupgradeneeded = (e) => { e.target.result.createObjectStore('wal'); };
        req.onsuccess = (e) => resolve(e.target.result);
        req.onerror   = (e) => reject(e.target.error);
    });
}
export function idbReadAll(db) {
    return new Promise((resolve, reject) => {
        const tx    = db.transaction('wal', 'readonly');
        const store = tx.objectStore('wal');
        let done = 0, keys, vals;
        const finish = () => { if (++done === 2) resolve([keys, vals]); };
        store.getAllKeys().onsuccess = (e) => { keys = e.target.result; finish(); };
        store.getAll().onsuccess    = (e) => { vals = e.target.result; finish(); };
        tx.onerror = (e) => reject(e.target.error);
    });
}
export function idbAppend(db, seq, cmd) {
    return new Promise((resolve, reject) => {
        const tx  = db.transaction('wal', 'readwrite');
        const req = tx.objectStore('wal').put(cmd, seq);
        req.onsuccess = () => resolve(undefined);
        req.onerror   = (e) => reject(e.target.error);
    });
}
export function idbClear(db) {
    return new Promise((resolve, reject) => {
        const tx  = db.transaction('wal', 'readwrite');
        const req = tx.objectStore('wal').clear();
        req.onsuccess = () => resolve(undefined);
        req.onerror   = (e) => reject(e.target.error);
    });
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = "openRecachedDb")]
    fn open_recached_db() -> Promise;
    #[wasm_bindgen(js_name = "idbReadAll")]
    fn idb_read_all(db: &JsValue) -> Promise;
    #[wasm_bindgen(js_name = "idbAppend")]
    fn idb_append_js(db: &JsValue, seq: f64, cmd: &str) -> Promise;
    #[wasm_bindgen(js_name = "idbClear")]
    fn idb_clear_js(db: &JsValue) -> Promise;
}

// ── RESP helper ───────────────────────────────────────────────────────────────

fn to_resp(parts: &[&str]) -> String {
    let mut s = format!("*{}\r\n", parts.len());
    for part in parts {
        s.push_str(&format!("${}\r\n{}\r\n", part.len(), part));
    }
    s
}

fn to_resp_owned(parts: &[String]) -> String {
    let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
    to_resp(&refs)
}

// ── WAL compaction ────────────────────────────────────────────────────────────

/// Compact after this many replayed WAL entries on load — rewrite as minimal
/// snapshot commands so the next replay is fast regardless of write history.
const WAL_COMPACT_THRESHOLD: u32 = 1000;

/// Convert snapshot entries into minimal RESP command strings suitable for
/// storing in the WAL. Each entry produces one command; entries with a TTL on
/// collection types produce an extra PEXPIREAT command.
fn snapshot_to_resp_cmds(entries: &[SnapshotEntry]) -> Vec<String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let mut out = Vec::new();
    for e in entries {
        if e.expires_at_ms.is_some_and(|exp| now_ms >= exp) {
            continue;
        }
        let data_parts: Vec<String> = match &e.value {
            SnapshotValue::Str(s) => {
                if let Some(exp) = e.expires_at_ms {
                    let rem_ms = exp.saturating_sub(now_ms);
                    vec![
                        "SET".into(),
                        e.key.clone(),
                        s.clone(),
                        "PX".into(),
                        rem_ms.to_string(),
                    ]
                } else {
                    vec!["SET".into(), e.key.clone(), s.clone()]
                }
            }
            SnapshotValue::Hash(map) => {
                if map.is_empty() {
                    continue;
                }
                let mut parts = vec!["HSET".to_string(), e.key.clone()];
                for (f, v) in map {
                    parts.push(f.clone());
                    parts.push(v.clone());
                }
                parts
            }
            SnapshotValue::List(list) => {
                if list.is_empty() {
                    continue;
                }
                let mut parts = vec!["RPUSH".to_string(), e.key.clone()];
                parts.extend(list.iter().cloned());
                parts
            }
            SnapshotValue::Set(set) => {
                if set.is_empty() {
                    continue;
                }
                let mut parts = vec!["SADD".to_string(), e.key.clone()];
                parts.extend(set.iter().cloned());
                parts
            }
            SnapshotValue::ZSet(pairs) => {
                if pairs.is_empty() {
                    continue;
                }
                let mut parts = vec!["ZADD".to_string(), e.key.clone()];
                for (member, score) in pairs {
                    parts.push(format_score(*score));
                    parts.push(member.clone());
                }
                parts
            }
            // Rate-limiter attempt state is transient and server-side; it is
            // not persisted in the browser WAL.
            SnapshotValue::RateLimiter { .. } => continue,
            SnapshotValue::Json(doc) => {
                vec!["JSET".into(), e.key.clone(), "$".into(), doc.clone()]
            }
        };
        out.push(to_resp_owned(&data_parts));
        // Non-string types with a TTL need a separate PEXPIREAT command.
        if !matches!(&e.value, SnapshotValue::Str(_))
            && let Some(exp) = e.expires_at_ms
        {
            out.push(to_resp_owned(&[
                "PEXPIREAT".to_string(),
                e.key.clone(),
                exp.to_string(),
            ]));
        }
    }
    out
}

// ── RecachedCache ─────────────────────────────────────────────────────────────

#[wasm_bindgen]
pub struct RecachedCache {
    store: Arc<KeyValueStore>,
    ws: Option<WebSocket>,
    bc: Option<BroadcastChannel>,
    /// Handle to the open IndexedDB; None until enable_persistence() resolves.
    idb: Rc<RefCell<Option<JsValue>>>,
    /// Monotonically-increasing WAL sequence counter.
    seq: Rc<Cell<u64>>,
    /// JS callback invoked after every mutation (local or server/BC-pushed).
    on_mutation: Rc<RefCell<Option<js_sys::Function>>>,
    /// JS callback invoked when a pub/sub message arrives: `cb(channel, message)`.
    on_message: Rc<RefCell<Option<js_sys::Function>>>,
    _onmessage: Option<Closure<dyn FnMut(MessageEvent)>>,
    _onbc: Option<Closure<dyn FnMut(MessageEvent)>>,
    _onopen: Option<Closure<dyn FnMut(Event)>>,
    /// Commands enqueued while the socket is still CONNECTING. Flushed in order
    /// by the `onopen` handler so AUTH and early writes are never dropped.
    ws_pending: Rc<RefCell<Vec<String>>>,
    /// True when connected via unencrypted ws:// (not wss://).
    ws_is_plaintext: bool,
}

impl Default for RecachedCache {
    fn default() -> Self {
        Self::new()
    }
}

// ── persistence helper ────────────────────────────────────────────────────────

fn persist_cmd(idb: &Rc<RefCell<Option<JsValue>>>, seq: &Rc<Cell<u64>>, encoded: &str) {
    let maybe_db = idb.borrow().as_ref().cloned();
    if let Some(db) = maybe_db {
        let s = seq.get();
        seq.set(s + 1);
        let cmd = encoded.to_string();
        spawn_local(async move {
            let _ = JsFuture::from(idb_append_js(&db, s as f64, &cmd)).await;
        });
    }
}

fn notify_mutation(on_mut: &Rc<RefCell<Option<js_sys::Function>>>) {
    if let Some(f) = on_mut.borrow().as_ref() {
        let _ = f.call0(&JsValue::NULL);
    }
}

/// Apply a server-initiated Array frame to the local store: `keychange`
/// pushes (WATCH / live queries) and `qstate` initial-state replies (QSUB).
/// Silently ignores anything else — ordinary command replies, acks, errors.
fn apply_server_array(
    store: &KeyValueStore,
    on_mut: &Rc<RefCell<Option<js_sys::Function>>>,
    items: Vec<Value>,
) {
    let tag = match items.first() {
        Some(Value::BulkString(Some(t))) => t.clone(),
        _ => return,
    };
    match tag.as_slice() {
        b"keychange" if items.len() == 3 => {
            let Value::BulkString(Some(key)) = &items[1] else {
                return;
            };
            let key = String::from_utf8_lossy(key).into_owned();
            match &items[2] {
                Value::BulkString(Some(v)) => {
                    store.execute(Command::Set(
                        key,
                        String::from_utf8_lossy(v).into_owned(),
                        SetOptions::default(),
                    ));
                }
                Value::BulkString(None) => {
                    store.execute(Command::Del(vec![key]));
                }
                // Collection-type marker: the value is not carried in the
                // keychange; the regular mutation push keeps collections in
                // sync — this frame only needs to trigger a re-render.
                _ => {}
            }
            notify_mutation(on_mut);
        }
        b"qstate" if items.len() >= 2 => {
            for pair in items[2..].chunks_exact(2) {
                let Value::BulkString(Some(k)) = &pair[0] else {
                    continue;
                };
                // String values are applied; collection-typed keys arrive as
                // type-name markers and are hydrated by mutation pushes.
                if let Value::BulkString(Some(v)) = &pair[1] {
                    store.execute(Command::Set(
                        String::from_utf8_lossy(k).into_owned(),
                        String::from_utf8_lossy(v).into_owned(),
                        SetOptions::default(),
                    ));
                }
            }
            notify_mutation(on_mut);
        }
        _ => {}
    }
}

// ── public API ────────────────────────────────────────────────────────────────

#[wasm_bindgen]
impl RecachedCache {
    #[wasm_bindgen(constructor)]
    pub fn new() -> RecachedCache {
        RecachedCache {
            store: Arc::new(KeyValueStore::new()),
            ws: None,
            bc: None,
            idb: Rc::new(RefCell::new(None)),
            seq: Rc::new(Cell::new(0)),
            on_mutation: Rc::new(RefCell::new(None)),
            on_message: Rc::new(RefCell::new(None)),
            _onmessage: None,
            _onbc: None,
            _onopen: None,
            ws_pending: Rc::new(RefCell::new(Vec::new())),
            ws_is_plaintext: false,
        }
    }

    /// Send `encoded` to the server if the socket is open, otherwise buffer it
    /// until `onopen` fires. Without this, anything sent during the CONNECTING
    /// window (notably the AUTH that `createCache` issues right after connect)
    /// would be silently dropped.
    fn ws_enqueue(&self, encoded: &str) {
        if let Some(ws) = &self.ws {
            if ws.ready_state() == WebSocket::OPEN {
                let _ = ws.send_with_str(encoded);
            } else {
                self.ws_pending.borrow_mut().push(encoded.to_string());
            }
        }
    }

    /// Register a JS callback invoked after every mutation from any source
    /// (local write, server WebSocket push, or BroadcastChannel sync).
    /// The SDK's `onMutation()` wires this up automatically.
    pub fn set_mutation_callback(&mut self, cb: js_sys::Function) {
        *self.on_mutation.borrow_mut() = Some(cb);
    }

    /// Register a JS callback invoked when a pub/sub message arrives.
    /// Signature: `cb(channel: string, message: string)`.
    /// The SDK's `onMessage()` wires this up automatically.
    pub fn set_message_callback(&mut self, cb: js_sys::Function) {
        *self.on_message.borrow_mut() = Some(cb);
    }

    /// Open the IndexedDB WAL, replay all stored commands into the in-memory
    /// store, and enable persistence for future writes.
    ///
    /// Call once at startup before reading or writing if you want the cache to
    /// survive page refreshes. Safe to call with no server connection.
    ///
    /// ```js
    /// await cache.enable_persistence();
    /// ```
    pub fn enable_persistence(&self) -> Promise {
        let store = Arc::clone(&self.store);
        let idb_cell = Rc::clone(&self.idb);
        let seq_cell = Rc::clone(&self.seq);

        wasm_bindgen_futures::future_to_promise(async move {
            let db = JsFuture::from(open_recached_db()).await?;

            let result = JsFuture::from(idb_read_all(&db)).await?;
            let pair = js_sys::Array::from(&result);
            let keys = js_sys::Array::from(&pair.get(0));
            let vals = js_sys::Array::from(&pair.get(1));

            let entry_count = keys.length();
            let mut max_seq: u64 = 0;
            for i in 0..entry_count {
                let s = keys.get(i).as_f64().unwrap_or(0.0) as u64;
                let cmd_str = vals.get(i).as_string().unwrap_or_default();
                if let Ok((value, _)) = Value::parse(cmd_str.as_bytes())
                    && let Ok(cmd) = Command::from_value(value)
                {
                    store.execute(cmd);
                    if s > max_seq {
                        max_seq = s;
                    }
                }
            }

            // If the WAL grew large, compact: rewrite it as minimal snapshot
            // commands. This keeps startup replay fast regardless of how many
            // writes accumulated between refreshes.
            // Note: there is a brief data-loss window between idb_clear_js and
            // writing the new snapshot. If the tab is closed during compaction,
            // the WAL will be empty on next load and in-memory state is lost.
            let next_seq = if entry_count > WAL_COMPACT_THRESHOLD {
                JsFuture::from(idb_clear_js(&db)).await?;
                let cmds = snapshot_to_resp_cmds(&store.snapshot());
                let mut seq: u64 = 0;
                for cmd_str in &cmds {
                    let _ = JsFuture::from(idb_append_js(&db, seq as f64, cmd_str)).await;
                    seq += 1;
                }
                seq
            } else if entry_count == 0 {
                0
            } else {
                max_seq + 1
            };

            seq_cell.set(next_seq);
            *idb_cell.borrow_mut() = Some(db);

            Ok(JsValue::UNDEFINED)
        })
    }

    /// Erase the IndexedDB WAL. The in-memory store is not affected.
    /// Useful for sign-out flows where you want to drop all persisted state.
    ///
    /// ```js
    /// await cache.clear_persistence();
    /// ```
    pub fn clear_persistence(&self) -> Promise {
        let maybe_db = self.idb.borrow().as_ref().cloned();
        if let Some(db) = maybe_db {
            wasm_bindgen_futures::future_to_promise(async move {
                JsFuture::from(idb_clear_js(&db)).await?;
                Ok(JsValue::UNDEFINED)
            })
        } else {
            Promise::resolve(&JsValue::UNDEFINED)
        }
    }

    /// Opt-in to cross-tab sync via the BroadcastChannel API.
    /// All tabs calling `broadcast()` with the same channel name share mutations
    /// automatically — no server required. Isolated by default (never called = no sync).
    /// Calling again with a different name replaces the previous channel cleanly.
    pub fn broadcast(&mut self, channel_name: &str) -> Result<(), JsValue> {
        let bc = BroadcastChannel::new(channel_name)?;
        let store_clone = Arc::clone(&self.store);
        let on_mut = Rc::clone(&self.on_mutation);

        let onbc = Closure::wrap(Box::new(move |e: MessageEvent| {
            if let Ok(text) = e.data().dyn_into::<js_sys::JsString>() {
                let s = String::from(text);
                if let Ok((value, _)) = Value::parse(s.as_bytes())
                    && let Ok(cmd) = Command::from_value(value)
                {
                    match cmd {
                        Command::Set(_, _, _)
                        | Command::Del(_)
                        | Command::Unlink(_)
                        | Command::MSet(_)
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
                        | Command::JMerge(_, _) => {
                            store_clone.execute(cmd);
                            notify_mutation(&on_mut);
                        }
                        _ => {}
                    }
                }
            }
        }) as Box<dyn FnMut(MessageEvent)>);

        bc.set_onmessage(Some(onbc.as_ref().unchecked_ref()));

        self._onbc = Some(onbc);
        self.bc = Some(bc);
        Ok(())
    }

    /// Connect to the native Recached backend via WebSockets.
    /// Calling this a second time cleanly replaces the previous connection.
    pub fn connect(&mut self, url: &str) -> Result<(), JsValue> {
        if let Some(old_ws) = self.ws.take() {
            let _ = old_ws.close();
        }
        self.ws_is_plaintext = url.starts_with("ws://");
        let ws = WebSocket::new(url)?;
        let store_clone = Arc::clone(&self.store);
        let on_mut = Rc::clone(&self.on_mutation);
        let on_msg = Rc::clone(&self.on_message);

        let onmessage = Closure::wrap(Box::new(move |e: MessageEvent| {
            if let Ok(text) = e.data().dyn_into::<js_sys::JsString>() {
                let s = String::from(text);
                // keychange pushes and qstate initial-state replies arrive as
                // plain Arrays (not RESP3 Push) — apply them to the local store.
                if let Ok((Value::Array(Some(items)), _)) = Value::parse(s.as_bytes()) {
                    apply_server_array(&store_clone, &on_mut, items);
                    return;
                }
                if let Ok((Value::Push(arr), _)) = Value::parse(s.as_bytes()) {
                    // Pub/sub message: >3 ["message", channel, payload]
                    if arr.len() == 3
                        && let (
                            Value::BulkString(Some(kind)),
                            Value::BulkString(Some(channel)),
                            Value::BulkString(Some(payload)),
                        ) = (&arr[0], &arr[1], &arr[2])
                        && kind.eq_ignore_ascii_case(b"message")
                    {
                        if let Some(f) = on_msg.borrow().as_ref() {
                            let ch = String::from_utf8_lossy(channel);
                            let pl = String::from_utf8_lossy(payload);
                            let _ = f.call2(
                                &JsValue::NULL,
                                &JsValue::from_str(&ch),
                                &JsValue::from_str(&pl),
                            );
                        }
                        return;
                    }
                    // Mutation push: convert to Array for command dispatch
                    if let Ok(cmd) = Command::from_value(Value::Array(Some(arr))) {
                        match cmd {
                            Command::Set(_, _, _)
                            | Command::Del(_)
                            | Command::Unlink(_)
                            | Command::MSet(_)
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
                            | Command::JMerge(_, _) => {
                                store_clone.execute(cmd);
                                notify_mutation(&on_mut);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }) as Box<dyn FnMut(MessageEvent)>);

        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

        // Flush commands buffered while the socket was CONNECTING (AUTH first,
        // then any early writes), preserving FIFO order.
        let pending = Rc::clone(&self.ws_pending);
        let ws_for_open = ws.clone();
        let onopen = Closure::wrap(Box::new(move |_e: Event| {
            for msg in pending.borrow_mut().drain(..) {
                let _ = ws_for_open.send_with_str(&msg);
            }
        }) as Box<dyn FnMut(Event)>);
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));

        self._onopen = Some(onopen);
        self._onmessage = Some(onmessage);
        self.ws = Some(ws);
        Ok(())
    }

    /// Send an AUTH command to the server. The response arrives asynchronously via onmessage.
    pub fn auth(&self, password: &str) -> String {
        if self.ws_is_plaintext {
            let _ = web_sys::console::warn_1(&JsValue::from_str(
                "recached: AUTH over unencrypted ws:// exposes the password in plaintext; use wss://",
            ));
        }
        self.ws_enqueue(&to_resp(&["AUTH", password]));
        "OK".to_string()
    }

    /// Present a signed sync-scope token (servers running with
    /// `RECACHED_SYNC_SECRET`). Scopes what this connection may read, write,
    /// and receive. The grant confirmation arrives asynchronously.
    pub fn sync_token(&self, token: &str) {
        self.ws_enqueue(&to_resp(&["SYNC", "TOKEN", token]));
    }

    /// Set sync scopes directly from comma-separated glob patterns. Only
    /// honoured by servers without a sync secret — a bandwidth filter, not an
    /// authorization boundary.
    pub fn sync_scopes(&self, patterns_csv: &str) {
        let pats: Vec<&str> = patterns_csv
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        if pats.is_empty() {
            return;
        }
        let mut parts = vec!["SYNC"];
        parts.extend(pats);
        self.ws_enqueue(&to_resp(&parts));
    }

    /// Subscribe to a live query. The server replies with the current state
    /// of every key matching the glob pattern — applied into the local store —
    /// then streams every change to matching keys. The mutation callback
    /// fires on the initial state and on each change; read the data with
    /// `get_matching`.
    pub fn live_query(&self, pattern: &str) {
        self.ws_enqueue(&to_resp(&["QSUB", pattern]));
    }

    /// Drop one live query, or all of them when `pattern` is omitted.
    pub fn live_unquery(&self, pattern: Option<String>) {
        match pattern {
            Some(p) => self.ws_enqueue(&to_resp(&["QUNSUB", &p])),
            None => self.ws_enqueue(&to_resp(&["QUNSUB"])),
        }
    }

    /// Set JSON at a path (`"$"` = whole document) — locally, on the server,
    /// and across tabs. `value` must be valid JSON text. Returns "OK" or an
    /// error string.
    pub fn jset(&self, key: &str, path: &str, value: &str) -> String {
        let resp = self.store.execute(Command::JSet(
            key.to_string(),
            path.to_string(),
            value.to_string(),
        ));
        if let Value::Error(e) = resp {
            return e;
        }
        let encoded = to_resp(&["JSET", key, path, value]);
        self.ws_enqueue(&encoded);
        if let Some(bc) = &self.bc {
            let _ = bc.post_message(&JsValue::from_str(&encoded));
        }
        persist_cmd(&self.idb, &self.seq, &encoded);
        notify_mutation(&self.on_mutation);
        "OK".to_string()
    }

    /// Read JSON at a path from the local store, serialized. `None` when the
    /// key or path does not exist.
    pub fn jget(&self, key: &str, path: Option<String>) -> Option<String> {
        match self
            .store
            .execute(Command::JGet(key.to_string(), path.clone()))
        {
            Value::BulkString(Some(bytes)) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            _ => None,
        }
    }

    /// RFC 7386 JSON Merge Patch against the whole document — locally, on the
    /// server, and across tabs. `null` fields remove keys; a `null` patch
    /// deletes the document. Returns "OK" or an error string.
    pub fn jmerge(&self, key: &str, patch: &str) -> String {
        let resp = self
            .store
            .execute(Command::JMerge(key.to_string(), patch.to_string()));
        if let Value::Error(e) = resp {
            return e;
        }
        let encoded = to_resp(&["JMERGE", key, patch]);
        self.ws_enqueue(&encoded);
        if let Some(bc) = &self.bc {
            let _ = bc.post_message(&JsValue::from_str(&encoded));
        }
        persist_cmd(&self.idb, &self.seq, &encoded);
        notify_mutation(&self.on_mutation);
        "OK".to_string()
    }

    /// Snapshot of local keys matching a glob pattern, as an array of
    /// `[key, value]` pairs sorted by key — deterministic order for UI
    /// rendering and stable snapshot comparison. String values only; keys
    /// holding collection types come back with `null` (read them with typed
    /// accessors).
    pub fn get_matching(&self, pattern: &str) -> js_sys::Array {
        let mut kvs = self.store.matching_key_values(pattern, usize::MAX);
        kvs.sort_by(|(a, _), (b, _)| a.cmp(b));
        let out = js_sys::Array::new();
        for (k, v) in kvs {
            let pair = js_sys::Array::new();
            pair.push(&JsValue::from_str(&k));
            match v {
                Value::BulkString(Some(bytes)) => {
                    pair.push(&JsValue::from_str(&String::from_utf8_lossy(&bytes)));
                }
                _ => {
                    pair.push(&JsValue::NULL);
                }
            }
            out.push(&pair);
        }
        out
    }

    /// Set a key-value pair locally, sync to the server, and fan out to other tabs if broadcast() was called.
    pub fn set(&self, key: &str, value: &str) -> String {
        let resp = self.store.execute(Command::Set(
            key.to_string(),
            value.to_string(),
            SetOptions::default(),
        ));

        let encoded = to_resp(&["SET", key, value]);
        self.ws_enqueue(&encoded);
        if let Some(bc) = &self.bc {
            let _ = bc.post_message(&JsValue::from_str(&encoded));
        }
        persist_cmd(&self.idb, &self.seq, &encoded);
        notify_mutation(&self.on_mutation);

        match resp {
            Value::SimpleString(s) => s,
            Value::Error(e) => e,
            _ => "ERR".to_string(),
        }
    }

    /// Set a key with a TTL in seconds, synced to the server and other tabs if broadcast() was called.
    pub fn set_ex(&self, key: &str, value: &str, seconds: u32) -> String {
        let opts = SetOptions {
            expiry: Some(core_engine::cmd::SetExpiry::Ex(seconds as u64)),
            ..Default::default()
        };
        let resp = self
            .store
            .execute(Command::Set(key.to_string(), value.to_string(), opts));

        let encoded = to_resp(&["SET", key, value, "EX", &seconds.to_string()]);
        self.ws_enqueue(&encoded);
        if let Some(bc) = &self.bc {
            let _ = bc.post_message(&JsValue::from_str(&encoded));
        }
        persist_cmd(&self.idb, &self.seq, &encoded);
        notify_mutation(&self.on_mutation);

        match resp {
            Value::SimpleString(s) => s,
            Value::Error(e) => e,
            _ => "ERR".to_string(),
        }
    }

    /// Get a value from the local store (zero latency).
    pub fn get(&self, key: &str) -> Option<String> {
        match self.store.execute(Command::Get(key.to_string())) {
            Value::BulkString(Some(data)) => Some(String::from_utf8_lossy(&data).into_owned()),
            _ => None,
        }
    }

    /// Delete a key locally, sync to the server, and fan out to other tabs if broadcast() was called.
    pub fn del(&self, key: &str) -> i32 {
        let resp = self.store.execute(Command::Del(vec![key.to_string()]));

        let encoded = to_resp(&["DEL", key]);
        self.ws_enqueue(&encoded);
        if let Some(bc) = &self.bc {
            let _ = bc.post_message(&JsValue::from_str(&encoded));
        }
        persist_cmd(&self.idb, &self.seq, &encoded);
        notify_mutation(&self.on_mutation);

        match resp {
            Value::Integer(i) => i as i32,
            _ => 0,
        }
    }

    /// Get the TTL of a key in seconds (-1 = no TTL, -2 = key doesn't exist).
    pub fn ttl(&self, key: &str) -> i32 {
        match self.store.execute(Command::Ttl(key.to_string())) {
            Value::Integer(n) => n as i32,
            _ => -2,
        }
    }

    /// Check if a key exists in the local store.
    pub fn exists(&self, key: &str) -> bool {
        matches!(
            self.store.execute(Command::Exists(vec![key.to_string()])),
            Value::Integer(1)
        )
    }

    /// Publish a message to a channel on the server.
    pub fn publish(&self, channel: &str, message: &str) {
        self.ws_enqueue(&to_resp(&["PUBLISH", channel, message]));
    }

    /// Subscribe to a channel on the server. Push messages arrive via the `onmessage` callback.
    pub fn subscribe(&self, channel: &str) {
        self.ws_enqueue(&to_resp(&["SUBSCRIBE", channel]));
    }

    /// Unsubscribe from a channel on the server.
    pub fn unsubscribe(&self, channel: &str) {
        self.ws_enqueue(&to_resp(&["UNSUBSCRIBE", channel]));
    }
}
