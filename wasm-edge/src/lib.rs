use core_engine::cmd::{Command, SetOptions};
use core_engine::resp::Value;
use core_engine::store::{KeyValueStore, SnapshotEntry, SnapshotValue, format_score};
use js_sys::Promise;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use sync_client::{Incoming, SyncClient, is_replayable_mutation, to_resp};
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
        // v2 added 'outbox' (writes awaiting server acknowledgment);
        // v3 adds 'meta' (client identity + session epoch for exactly-once).
        const req = indexedDB.open('recached', 3);
        req.onupgradeneeded = (e) => {
            const db = e.target.result;
            if (!db.objectStoreNames.contains('wal')) db.createObjectStore('wal');
            if (!db.objectStoreNames.contains('outbox')) db.createObjectStore('outbox');
            if (!db.objectStoreNames.contains('meta')) db.createObjectStore('meta');
        };
        req.onsuccess = (e) => resolve(e.target.result);
        req.onerror   = (e) => reject(e.target.error);
    });
}
function readAll(db, name) {
    return new Promise((resolve, reject) => {
        const tx    = db.transaction(name, 'readonly');
        const store = tx.objectStore(name);
        let done = 0, keys, vals;
        const finish = () => { if (++done === 2) resolve([keys, vals]); };
        store.getAllKeys().onsuccess = (e) => { keys = e.target.result; finish(); };
        store.getAll().onsuccess    = (e) => { vals = e.target.result; finish(); };
        tx.onerror = (e) => reject(e.target.error);
    });
}
export function idbReadAll(db) { return readAll(db, 'wal'); }
export function idbOutboxReadAll(db) { return readAll(db, 'outbox'); }
export function idbAppend(db, seq, cmd) {
    return new Promise((resolve, reject) => {
        const tx  = db.transaction('wal', 'readwrite');
        const req = tx.objectStore('wal').put(cmd, seq);
        req.onsuccess = () => resolve(undefined);
        req.onerror   = (e) => reject(e.target.error);
    });
}
export function idbOutboxPut(db, id, cmd) {
    return new Promise((resolve, reject) => {
        const tx  = db.transaction('outbox', 'readwrite');
        const req = tx.objectStore('outbox').put(cmd, id);
        req.onsuccess = () => resolve(undefined);
        req.onerror   = (e) => reject(e.target.error);
    });
}
export function idbOutboxDelete(db, id) {
    return new Promise((resolve, reject) => {
        const tx  = db.transaction('outbox', 'readwrite');
        const req = tx.objectStore('outbox').delete(id);
        req.onsuccess = () => resolve(undefined);
        req.onerror   = (e) => reject(e.target.error);
    });
}
export function idbWalClear(db) {
    return new Promise((resolve, reject) => {
        const tx  = db.transaction('wal', 'readwrite');
        const req = tx.objectStore('wal').clear();
        req.onsuccess = () => resolve(undefined);
        req.onerror   = (e) => reject(e.target.error);
    });
}
export function idbMetaGet(db, key) {
    return new Promise((resolve, reject) => {
        const tx  = db.transaction('meta', 'readonly');
        const req = tx.objectStore('meta').get(key);
        req.onsuccess = (e) => resolve(e.target.result);
        req.onerror   = (e) => reject(e.target.error);
    });
}
export function idbMetaPut(db, key, val) {
    return new Promise((resolve, reject) => {
        const tx  = db.transaction('meta', 'readwrite');
        const req = tx.objectStore('meta').put(val, key);
        req.onsuccess = () => resolve(undefined);
        req.onerror   = (e) => reject(e.target.error);
    });
}
export function idbClear(db) {
    return new Promise((resolve, reject) => {
        const tx  = db.transaction(['wal', 'outbox'], 'readwrite');
        tx.objectStore('wal').clear();
        tx.objectStore('outbox').clear();
        tx.oncomplete = () => resolve(undefined);
        tx.onerror    = (e) => reject(e.target.error);
    });
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = "openRecachedDb")]
    fn open_recached_db() -> Promise;
    #[wasm_bindgen(js_name = "idbReadAll")]
    fn idb_read_all(db: &JsValue) -> Promise;
    #[wasm_bindgen(js_name = "idbOutboxReadAll")]
    fn idb_outbox_read_all(db: &JsValue) -> Promise;
    #[wasm_bindgen(js_name = "idbAppend")]
    fn idb_append_js(db: &JsValue, seq: f64, cmd: &str) -> Promise;
    #[wasm_bindgen(js_name = "idbOutboxPut")]
    fn idb_outbox_put_js(db: &JsValue, id: f64, cmd: &str) -> Promise;
    #[wasm_bindgen(js_name = "idbOutboxDelete")]
    fn idb_outbox_delete_js(db: &JsValue, id: f64) -> Promise;
    #[wasm_bindgen(js_name = "idbWalClear")]
    fn idb_wal_clear_js(db: &JsValue) -> Promise;
    #[wasm_bindgen(js_name = "idbMetaGet")]
    fn idb_meta_get(db: &JsValue, key: &str) -> Promise;
    #[wasm_bindgen(js_name = "idbMetaPut")]
    fn idb_meta_put(db: &JsValue, key: &str, val: &JsValue) -> Promise;
    #[wasm_bindgen(js_name = "idbClear")]
    fn idb_clear_js(db: &JsValue) -> Promise;
}

// ── RESP helper ───────────────────────────────────────────────────────────────

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

/// Unguessable client id: `crypto.randomUUID()` when available (unguessability
/// is what stops another authenticated client from poisoning this client's
/// dedup high-water mark), with a Math.random+timestamp fallback.
fn random_client_id() -> String {
    if let Some(w) = web_sys::window()
        && let Ok(c) = js_sys::Reflect::get(&w, &JsValue::from_str("crypto"))
        && !c.is_undefined()
        && let Ok(f) = js_sys::Reflect::get(&c, &JsValue::from_str("randomUUID"))
        && f.is_function()
        && let Ok(v) = js_sys::Function::from(f).call0(&c)
        && let Some(s) = v.as_string()
    {
        return s;
    }
    format!(
        "{:08x}{:08x}{:08x}-{:x}",
        (js_sys::Math::random() * u32::MAX as f64) as u32,
        (js_sys::Math::random() * u32::MAX as f64) as u32,
        (js_sys::Math::random() * u32::MAX as f64) as u32,
        js_sys::Date::now() as u64,
    )
}

// ── WebSocket connection state ────────────────────────────────────────────────
//
// The sync logic itself — outbox, dedup envelopes, reply/ack correlation,
// session re-establishment, backoff — lives in the platform-neutral
// `sync-client` crate (shared with the planned mobile bindings). This layer
// only adapts it to the browser: WebSocket I/O, IndexedDB persistence,
// setTimeout reconnect timers, and JS callbacks.

/// Event closures for the *current* socket. Replaced wholesale on reconnect;
/// dropping the old ones detaches them.
#[derive(Default)]
struct WsHandlers {
    onmessage: Option<Closure<dyn FnMut(MessageEvent)>>,
    onopen: Option<Closure<dyn FnMut(Event)>>,
    onclose: Option<Closure<dyn FnMut(Event)>>,
}

/// Everything the reconnect machinery needs, clonable into JS closures.
/// The socket itself lives behind `Rc<RefCell<…>>` so a reconnect fired from
/// an event handler can replace it.
#[derive(Clone)]
struct WsShared {
    /// The platform-neutral sync state machine.
    core: Rc<RefCell<SyncClient>>,
    on_mutation: Rc<RefCell<Option<js_sys::Function>>>,
    on_message: Rc<RefCell<Option<js_sys::Function>>>,
    ws: Rc<RefCell<Option<WebSocket>>>,
    handlers: Rc<RefCell<WsHandlers>>,
    /// Shared with `RecachedCache.idb` — the open IndexedDB handle, if any.
    idb: Rc<RefCell<Option<JsValue>>>,
    url: Rc<RefCell<Option<String>>>,
    auto_reconnect: Rc<Cell<bool>>,
    /// Socket generation: bumped by `open_socket`. Stale sockets' close events
    /// (e.g. one replaced by an explicit `connect`) must not schedule
    /// reconnects over the live socket.
    generation: Rc<Cell<u64>>,
    /// Keeps the pending reconnect timer's callback alive until it fires.
    reconnect_cb: Rc<RefCell<Option<Closure<dyn FnMut()>>>>,
}

/// Delete an acknowledged (or evicted) outbox row from durable storage.
fn outbox_delete(sh: &WsShared, id: u64) {
    if let Some(db) = sh.idb.borrow().clone() {
        spawn_local(async move {
            let _ = JsFuture::from(idb_outbox_delete_js(&db, id as f64)).await;
        });
    }
}

/// Persist an outbox row to durable storage.
fn outbox_put(sh: &WsShared, id: u64, frame: &str) {
    if let Some(db) = sh.idb.borrow().clone() {
        let cmd = frame.to_string();
        spawn_local(async move {
            let _ = JsFuture::from(idb_outbox_put_js(&db, id as f64, &cmd)).await;
        });
    }
}

/// Execute the effects the core requested for an incoming frame.
fn dispatch_incoming(sh: &WsShared, incoming: Incoming) {
    match incoming {
        Incoming::Applied => notify_mutation(&sh.on_mutation),
        Incoming::PubSub { channel, message } => {
            if let Some(f) = sh.on_message.borrow().as_ref() {
                let _ = f.call2(
                    &JsValue::NULL,
                    &JsValue::from_str(&channel),
                    &JsValue::from_str(&message),
                );
            }
        }
        Incoming::Reply { retired } => {
            if let Some(id) = retired {
                outbox_delete(sh, id);
            }
        }
        Incoming::AppliedReply { retired } => {
            if let Some(id) = retired {
                outbox_delete(sh, id);
            }
            notify_mutation(&sh.on_mutation);
        }
        Incoming::Ignored => {}
    }
}

/// Queue a write through the core and mirror its effects: durable outbox row,
/// possible overflow eviction, and an immediate send when connected.
fn queue_write(sh: &WsShared, encoded: &str, dedup: bool) {
    // Local-only mode (no connect() and no persistence): nothing to sync to.
    if sh.url.borrow().is_none() && sh.idb.borrow().is_none() {
        return;
    }
    let connected = matches!(
        sh.ws.borrow().as_ref(),
        Some(ws) if ws.ready_state() == WebSocket::OPEN
    );
    let e = sh
        .core
        .borrow_mut()
        .enqueue_write(encoded, dedup, connected);
    if let Some(old) = e.dropped {
        outbox_delete(sh, old);
        let _ = web_sys::console::warn_1(&JsValue::from_str(
            "recached: offline write queue full — dropped the oldest queued write",
        ));
    }
    outbox_put(sh, e.id, &e.frame);
    if e.send_now
        && let Some(ws) = sh.ws.borrow().as_ref()
    {
        let _ = ws.send_with_str(&e.frame);
    }
}

/// Open a socket to the stored URL and wire its event handlers. Called on
/// `connect()` and again by the backoff timer after a drop.
fn open_socket(sh: &WsShared) {
    let Some(url) = sh.url.borrow().clone() else {
        return;
    };
    let generation = sh.generation.get() + 1;
    sh.generation.set(generation);

    let ws = match WebSocket::new(&url) {
        Ok(w) => w,
        Err(_) => {
            schedule_reconnect(sh);
            return;
        }
    };

    // ── onmessage: classify + apply via the core, execute its effects ────
    let sh_msg = sh.clone();
    let onmessage = Closure::wrap(Box::new(move |e: MessageEvent| {
        if let Ok(text) = e.data().dyn_into::<js_sys::JsString>() {
            let incoming = sh_msg.core.borrow_mut().handle_frame(&String::from(text));
            dispatch_incoming(&sh_msg, incoming);
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

    // ── onopen: the core decides what to send (session, then outbox) ─────
    let sh_open = sh.clone();
    let ws_for_open = ws.clone();
    let onopen = Closure::wrap(Box::new(move |_e: Event| {
        for frame in sh_open.core.borrow_mut().on_open() {
            let _ = ws_for_open.send_with_str(&frame);
        }
    }) as Box<dyn FnMut(Event)>);
    ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));

    // ── onclose: reconnect with backoff (unless this socket is stale) ────
    let sh_close = sh.clone();
    let onclose = Closure::wrap(Box::new(move |_e: Event| {
        if sh_close.generation.get() == generation {
            schedule_reconnect(&sh_close);
        }
    }) as Box<dyn FnMut(Event)>);
    ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));

    let mut h = sh.handlers.borrow_mut();
    h.onmessage = Some(onmessage);
    h.onopen = Some(onopen);
    h.onclose = Some(onclose);
    *sh.ws.borrow_mut() = Some(ws);
}

/// Schedule an `open_socket` retry at the core's backoff delay. No-op when
/// auto-reconnect is off or there is no window (non-browser contexts).
fn schedule_reconnect(sh: &WsShared) {
    if !sh.auto_reconnect.get() {
        return;
    }
    let delay = sh.core.borrow_mut().on_close();
    let Some(window) = web_sys::window() else {
        return;
    };
    let sh2 = sh.clone();
    let cb = Closure::wrap(Box::new(move || {
        if sh2.auto_reconnect.get() {
            open_socket(&sh2);
        }
    }) as Box<dyn FnMut()>);
    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        cb.as_ref().unchecked_ref(),
        delay as i32,
    );
    *sh.reconnect_cb.borrow_mut() = Some(cb);
}

// ── RecachedCache ─────────────────────────────────────────────────────────────

#[wasm_bindgen]
pub struct RecachedCache {
    store: Arc<KeyValueStore>,
    bc: Option<BroadcastChannel>,
    /// Handle to the open IndexedDB; None until enable_persistence() resolves.
    idb: Rc<RefCell<Option<JsValue>>>,
    /// Monotonically-increasing WAL sequence counter.
    seq: Rc<Cell<u64>>,
    /// JS callback invoked after every mutation (local or server/BC-pushed).
    on_mutation: Rc<RefCell<Option<js_sys::Function>>>,
    /// JS callback invoked when a pub/sub message arrives: `cb(channel, message)`.
    on_message: Rc<RefCell<Option<js_sys::Function>>>,
    _onbc: Option<Closure<dyn FnMut(MessageEvent)>>,
    /// Shared WebSocket connection state (socket, session, offline queue).
    shared: WsShared,
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

// ── public API ────────────────────────────────────────────────────────────────

#[wasm_bindgen]
impl RecachedCache {
    #[wasm_bindgen(constructor)]
    pub fn new() -> RecachedCache {
        let store = Arc::new(KeyValueStore::new());
        let on_mutation = Rc::new(RefCell::new(None));
        let on_message = Rc::new(RefCell::new(None));
        let idb = Rc::new(RefCell::new(None));
        let core = Rc::new(RefCell::new(SyncClient::new(
            Arc::clone(&store),
            random_client_id(),
        )));
        RecachedCache {
            shared: WsShared {
                core,
                on_mutation: Rc::clone(&on_mutation),
                on_message: Rc::clone(&on_message),
                ws: Rc::new(RefCell::new(None)),
                handlers: Rc::new(RefCell::new(WsHandlers::default())),
                idb: Rc::clone(&idb),
                url: Rc::new(RefCell::new(None)),
                auto_reconnect: Rc::new(Cell::new(false)),
                generation: Rc::new(Cell::new(0)),
                reconnect_cb: Rc::new(RefCell::new(None)),
            },
            store,
            bc: None,
            idb,
            seq: Rc::new(Cell::new(0)),
            on_mutation,
            on_message,
            _onbc: None,
            ws_is_plaintext: false,
        }
    }

    /// Queue a store write (dedup-wrapped for exactly-once delivery).
    fn ws_enqueue(&self, encoded: &str) {
        queue_write(&self.shared, encoded, true);
    }

    /// Queue a connection-scoped command (pub/sub) — replayed on reconnect
    /// but never dedup-wrapped: skipping a re-sent SUBSCRIBE would silently
    /// drop the subscription.
    fn ws_enqueue_nodedup(&self, encoded: &str) {
        queue_write(&self.shared, encoded, false);
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
        let core = Rc::clone(&self.shared.core);

        wasm_bindgen_futures::future_to_promise(async move {
            let db = JsFuture::from(open_recached_db()).await?;

            // ── exactly-once identity ─────────────────────────────────────
            // Adopt the stored client id (so dedup high-water marks span
            // sessions) unless this session already sent writes under the
            // random one — switching ids mid-stream would fragment the mark.
            {
                let stored = JsFuture::from(idb_meta_get(&db, "client_id")).await?;
                let adopted = match stored.as_string() {
                    Some(cid) if !cid.is_empty() => core.borrow_mut().adopt_client_id(cid),
                    _ => false,
                };
                if !adopted {
                    let current = core.borrow().client_id().to_string();
                    let _ = JsFuture::from(idb_meta_put(
                        &db,
                        "client_id",
                        &JsValue::from_str(&current),
                    ))
                    .await;
                }
                // Bump the session epoch so this session's dedup ids are
                // strictly above every id the previous session ever sent.
                let prev = JsFuture::from(idb_meta_get(&db, "epoch"))
                    .await?
                    .as_f64()
                    .unwrap_or(0.0) as u32;
                let next = prev.saturating_add(1);
                core.borrow_mut().set_epoch(next);
                let _ = JsFuture::from(idb_meta_put(&db, "epoch", &JsValue::from_f64(next as f64)))
                    .await;
            }

            // Restore the durable outbox: writes from a previous session that
            // never got a server acknowledgment. They re-send on the next
            // (re)connect, *before* writes queued in this session. Restored
            // entries are renumbered with fresh ids so they can never collide
            // with ids handed out in this session (replay order comes from
            // deque position, not from the id).
            {
                let result = JsFuture::from(idb_outbox_read_all(&db)).await?;
                let pair = js_sys::Array::from(&result);
                let keys = js_sys::Array::from(&pair.get(0));
                let vals = js_sys::Array::from(&pair.get(1));
                let mut restored: Vec<(u64, String)> = Vec::with_capacity(keys.length() as usize);
                for i in 0..keys.length() {
                    let id = keys.get(i).as_f64().unwrap_or(0.0) as u64;
                    let cmd = vals.get(i).as_string().unwrap_or_default();
                    if !cmd.is_empty() {
                        restored.push((id, cmd));
                    }
                }
                for (old_id, new_id, frame) in core.borrow_mut().restore_outbox(restored) {
                    if new_id != old_id {
                        let _ = JsFuture::from(idb_outbox_delete_js(&db, old_id as f64)).await;
                        let _ = JsFuture::from(idb_outbox_put_js(&db, new_id as f64, &frame)).await;
                    }
                }
            }

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
                // WAL only — the outbox holds writes still awaiting server
                // acknowledgment and must survive compaction.
                JsFuture::from(idb_wal_clear_js(&db)).await?;
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
        // Sign-out semantics: unsent offline writes are discarded too.
        self.shared.core.borrow_mut().clear_outbox();
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
                    && is_replayable_mutation(&cmd)
                {
                    store_clone.execute(cmd);
                    notify_mutation(&on_mut);
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
        self.ws_is_plaintext = url.starts_with("ws://");
        // Silence the old socket's close handler before replacing it — its
        // generation is now stale, so it cannot schedule a reconnect over the
        // new one, but closing it early keeps things tidy.
        self.shared.auto_reconnect.set(false);
        if let Some(old_ws) = self.shared.ws.borrow_mut().take() {
            let _ = old_ws.close();
        }
        *self.shared.url.borrow_mut() = Some(url.to_string());
        self.shared.auto_reconnect.set(true);
        open_socket(&self.shared);
        Ok(())
    }

    /// Close the connection and stop reconnecting. Local reads and writes keep
    /// working; writes queue and are replayed on the next `connect`.
    pub fn disconnect(&mut self) {
        self.shared.auto_reconnect.set(false);
        if let Some(ws) = self.shared.ws.borrow_mut().take() {
            let _ = ws.close();
        }
    }

    /// Enable or disable automatic reconnection (enabled by default once
    /// `connect` is called).
    pub fn set_auto_reconnect(&self, enabled: bool) {
        self.shared.auto_reconnect.set(enabled);
    }

    /// Send a session command now if the socket is open; otherwise it will be
    /// sent by `onopen` from the stored session state — session commands must
    /// not sit in the offline write queue or they would be sent twice.
    /// True when the socket exists and is OPEN.
    fn socket_open(&self) -> bool {
        matches!(
            self.shared.ws.borrow().as_ref(),
            Some(ws) if ws.ready_state() == WebSocket::OPEN
        )
    }

    /// Send a session frame the core produced (it has already recorded the
    /// inflight reply slot).
    fn send_session_frame(&self, frame: Option<String>) {
        if let Some(frame) = frame
            && let Some(ws) = self.shared.ws.borrow().as_ref()
        {
            let _ = ws.send_with_str(&frame);
        }
    }

    /// Send an AUTH command to the server. The password is remembered and
    /// re-sent automatically on every reconnect. The response arrives
    /// asynchronously via onmessage.
    pub fn auth(&self, password: &str) -> String {
        if self.ws_is_plaintext {
            let _ = web_sys::console::warn_1(&JsValue::from_str(
                "recached: AUTH over unencrypted ws:// exposes the password in plaintext; use wss://",
            ));
        }
        let open = self.socket_open();
        let frame = self.shared.core.borrow_mut().set_password(password, open);
        self.send_session_frame(frame);
        "OK".to_string()
    }

    /// Present a signed sync-scope token (servers running with
    /// `RECACHED_SYNC_SECRET`). The token is remembered and re-presented
    /// automatically on every reconnect. The grant confirmation arrives
    /// asynchronously.
    pub fn sync_token(&self, token: &str) {
        let open = self.socket_open();
        let frame = self.shared.core.borrow_mut().set_sync_token(token, open);
        self.send_session_frame(frame);
    }

    /// Set sync scopes directly from comma-separated glob patterns. Only
    /// honoured by servers without a sync secret — a bandwidth filter, not an
    /// authorization boundary. Re-applied automatically on reconnect.
    pub fn sync_scopes(&self, patterns_csv: &str) {
        let open = self.socket_open();
        let frame = self
            .shared
            .core
            .borrow_mut()
            .set_sync_scopes(patterns_csv, open);
        self.send_session_frame(frame);
    }

    /// Subscribe to a live query. The server replies with the current state
    /// of every key matching the glob pattern — applied into the local store —
    /// then streams every change to matching keys. Re-subscribed automatically
    /// on reconnect, which also re-hydrates the matching keys. The mutation
    /// callback fires on the initial state and on each change; read the data
    /// with `get_matching`.
    pub fn live_query(&self, pattern: &str) {
        let open = self.socket_open();
        let frame = self.shared.core.borrow_mut().add_live_query(pattern, open);
        self.send_session_frame(frame);
    }

    /// Drop one live query, or all of them when `pattern` is omitted.
    pub fn live_unquery(&self, pattern: Option<String>) {
        let open = self.socket_open();
        let frame = self
            .shared
            .core
            .borrow_mut()
            .remove_live_query(pattern.as_deref(), open);
        self.send_session_frame(frame);
    }

    /// Increment (or with a negative delta, decrement) an integer counter —
    /// locally, on the server, and across tabs. Returns the new local value.
    ///
    /// Offline increments queue as *deltas* and merge additively with
    /// concurrent increments from other clients on reconnect (PN-counter
    /// semantics) — unlike a plain `set`, nobody's increments are lost.
    pub fn incr_by(&self, key: &str, delta: i64) -> Result<i64, JsValue> {
        let resp = self.store.execute(Command::IncrBy(key.to_string(), delta));
        let n = match resp {
            Value::Integer(n) => n,
            Value::Error(e) => return Err(JsValue::from_str(&e)),
            _ => 0,
        };
        let delta_s = delta.to_string();
        let encoded = to_resp(&["INCRBY", key, &delta_s]);
        self.ws_enqueue(&encoded);
        if let Some(bc) = &self.bc {
            let _ = bc.post_message(&JsValue::from_str(&encoded));
        }
        persist_cmd(&self.idb, &self.seq, &encoded);
        notify_mutation(&self.on_mutation);
        Ok(n)
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
        self.ws_enqueue_nodedup(&to_resp(&["PUBLISH", channel, message]));
    }

    /// Subscribe to a channel on the server. Push messages arrive via the `onmessage` callback.
    pub fn subscribe(&self, channel: &str) {
        self.ws_enqueue_nodedup(&to_resp(&["SUBSCRIBE", channel]));
    }

    /// Unsubscribe from a channel on the server.
    pub fn unsubscribe(&self, channel: &str) {
        self.ws_enqueue_nodedup(&to_resp(&["UNSUBSCRIBE", channel]));
    }
}
