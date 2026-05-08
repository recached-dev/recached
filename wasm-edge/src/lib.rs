use core_engine::cmd::{Command, SetOptions};
use core_engine::resp::Value;
use core_engine::store::KeyValueStore;
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use web_sys::{BroadcastChannel, MessageEvent, WebSocket};

/// Encodes parts as a RESP bulk-string array, e.g. `["SET","k","v"]` → `*3\r\n$3\r\nSET\r\n…`.
fn to_resp(parts: &[&str]) -> String {
    let mut s = format!("*{}\r\n", parts.len());
    for part in parts {
        s.push_str(&format!("${}\r\n{}\r\n", part.len(), part));
    }
    s
}

#[wasm_bindgen]
pub struct RecachedCache {
    store: Arc<KeyValueStore>,
    ws: Option<WebSocket>,
    bc: Option<BroadcastChannel>,
    // Closures held here so they are dropped (and JS callbacks unregistered) on reconnect/re-broadcast.
    _onmessage: Option<Closure<dyn FnMut(MessageEvent)>>,
    _onbc: Option<Closure<dyn FnMut(MessageEvent)>>,
}

impl Default for RecachedCache {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl RecachedCache {
    #[wasm_bindgen(constructor)]
    pub fn new() -> RecachedCache {
        RecachedCache {
            store: Arc::new(KeyValueStore::new()),
            ws: None,
            bc: None,
            _onmessage: None,
            _onbc: None,
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

        // Incoming messages from the server are RESP-encoded mutation commands.
        let onmessage = Closure::wrap(Box::new(move |e: MessageEvent| {
            if let Ok(text) = e.data().dyn_into::<js_sys::JsString>() {
                let s = String::from(text);
                if let Ok((value, _)) = Value::parse(s.as_bytes())
                    && let Ok(cmd) = Command::from_value(value)
                {
                    match cmd {
                            // Strings + expiry
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
                            // Hash
                            | Command::HSet(_, _)
                            | Command::HDel(_, _)
                            | Command::HSetNx(_, _, _)
                            // List
                            | Command::LPush(_, _)
                            | Command::RPush(_, _)
                            | Command::LPop(_, _)
                            | Command::RPop(_, _)
                            | Command::LSet(_, _, _)
                            | Command::LRem(_, _, _)
                            | Command::LTrim(_, _, _)
                            // Set
                            | Command::SAdd(_, _)
                            | Command::SRem(_, _)
                            | Command::SMove(_, _, _)
                            | Command::SInterStore(_, _)
                            | Command::SUnionStore(_, _)
                            | Command::SDiffStore(_, _)
                            // Sorted Set
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

        // Store the closure in the struct — this keeps it alive and drops the old one.
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
    /// Returns the number of subscribers that received the message (as reported by the server).
    /// The response is asynchronous — the return value here is always 0 since the
    /// actual subscriber count arrives via the WebSocket response frame, not this call.
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
