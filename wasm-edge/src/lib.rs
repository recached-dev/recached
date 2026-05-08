use core_engine::cmd::{Command, SetOptions};
use core_engine::resp::Value;
use core_engine::store::KeyValueStore;
use js_sys::Promise;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{BroadcastChannel, MessageEvent, WebSocket};

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
    _onmessage: Option<Closure<dyn FnMut(MessageEvent)>>,
    _onbc: Option<Closure<dyn FnMut(MessageEvent)>>,
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
            _onmessage: None,
            _onbc: None,
        }
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

            let mut max_seq: u64 = 0;
            for i in 0..keys.length() {
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

            // If WAL was empty, start seq at 0; otherwise continue from max_seq + 1.
            seq_cell.set(if keys.length() == 0 { 0 } else { max_seq + 1 });
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
                        | Command::ZIncrBy(_, _, _) => {
                            store_clone.execute(cmd);
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
        let ws = WebSocket::new(url)?;
        let store_clone = Arc::clone(&self.store);

        let onmessage = Closure::wrap(Box::new(move |e: MessageEvent| {
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
                        | Command::ZIncrBy(_, _, _) => {
                            store_clone.execute(cmd);
                        }
                        _ => {}
                    }
                }
            }
        }) as Box<dyn FnMut(MessageEvent)>);

        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

        self._onmessage = Some(onmessage);
        self.ws = Some(ws);
        Ok(())
    }

    /// Send an AUTH command to the server. The response arrives asynchronously via onmessage.
    pub fn auth(&self, password: &str) -> String {
        if let Some(ws) = &self.ws
            && ws.ready_state() == WebSocket::OPEN
        {
            let _ = ws.send_with_str(&to_resp(&["AUTH", password]));
        }
        "OK".to_string()
    }

    /// Set a key-value pair locally, sync to the server, and fan out to other tabs if broadcast() was called.
    pub fn set(&self, key: &str, value: &str) -> String {
        let resp = self.store.execute(Command::Set(
            key.to_string(),
            value.to_string(),
            SetOptions::default(),
        ));

        let encoded = to_resp(&["SET", key, value]);
        if let Some(ws) = &self.ws
            && ws.ready_state() == WebSocket::OPEN
        {
            let _ = ws.send_with_str(&encoded);
        }
        if let Some(bc) = &self.bc {
            let _ = bc.post_message(&JsValue::from_str(&encoded));
        }
        persist_cmd(&self.idb, &self.seq, &encoded);

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
        if let Some(ws) = &self.ws
            && ws.ready_state() == WebSocket::OPEN
        {
            let _ = ws.send_with_str(&encoded);
        }
        if let Some(bc) = &self.bc {
            let _ = bc.post_message(&JsValue::from_str(&encoded));
        }
        persist_cmd(&self.idb, &self.seq, &encoded);

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
        if let Some(ws) = &self.ws
            && ws.ready_state() == WebSocket::OPEN
        {
            let _ = ws.send_with_str(&encoded);
        }
        if let Some(bc) = &self.bc {
            let _ = bc.post_message(&JsValue::from_str(&encoded));
        }
        persist_cmd(&self.idb, &self.seq, &encoded);

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
        if let Some(ws) = &self.ws
            && ws.ready_state() == WebSocket::OPEN
        {
            let _ = ws.send_with_str(&to_resp(&["PUBLISH", channel, message]));
        }
    }

    /// Subscribe to a channel on the server. Push messages arrive via the `onmessage` callback.
    pub fn subscribe(&self, channel: &str) {
        if let Some(ws) = &self.ws
            && ws.ready_state() == WebSocket::OPEN
        {
            let _ = ws.send_with_str(&to_resp(&["SUBSCRIBE", channel]));
        }
    }

    /// Unsubscribe from a channel on the server.
    pub fn unsubscribe(&self, channel: &str) {
        if let Some(ws) = &self.ws
            && ws.ready_state() == WebSocket::OPEN
        {
            let _ = ws.send_with_str(&to_resp(&["UNSUBSCRIBE", channel]));
        }
    }
}
