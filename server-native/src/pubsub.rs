//! Pub/sub: the subscriber hub for channels and patterns, and the wire
//! encoding of the messages and acknowledgements it produces.

use crate::*;

pub(crate) enum PubSubMsg {
    Message {
        channel: String,
        message: Vec<u8>,
    },
    PMessage {
        pattern: String,
        channel: String,
        message: Vec<u8>,
    },
}

pub(crate) type PubSubSender = mpsc::UnboundedSender<PubSubMsg>;

pub(crate) struct PubSubHub {
    pub(crate) channel_subs: HashMap<String, Vec<(u64, PubSubSender)>>,
    pub(crate) pattern_subs: Vec<(String, u64, PubSubSender)>,
}

impl PubSubHub {
    pub(crate) fn new() -> Self {
        Self {
            channel_subs: HashMap::new(),
            pattern_subs: Vec::new(),
        }
    }

    pub(crate) fn subscribe(&mut self, conn_id: u64, channel: &str, tx: PubSubSender) {
        self.channel_subs
            .entry(channel.to_string())
            .or_default()
            .push((conn_id, tx));
    }

    pub(crate) fn psubscribe(&mut self, conn_id: u64, pattern: &str, tx: PubSubSender) {
        self.pattern_subs.push((pattern.to_string(), conn_id, tx));
    }

    pub(crate) fn unsubscribe(&mut self, conn_id: u64, channel: &str) {
        if let Some(v) = self.channel_subs.get_mut(channel) {
            v.retain(|(id, _)| *id != conn_id);
            if v.is_empty() {
                self.channel_subs.remove(channel);
            }
        }
    }

    pub(crate) fn punsubscribe(&mut self, conn_id: u64, pattern: &str) {
        self.pattern_subs
            .retain(|(p, id, _)| !(p == pattern && *id == conn_id));
    }

    pub(crate) fn unsubscribe_all(&mut self, conn_id: u64) {
        self.channel_subs.retain(|_, v| {
            v.retain(|(id, _)| *id != conn_id);
            !v.is_empty()
        });
        self.pattern_subs.retain(|(_, id, _)| *id != conn_id);
    }

    /// Channels with at least one live subscriber, for `PUBSUB CHANNELS`.
    ///
    /// `unsubscribe` and `unsubscribe_all` remove a channel's entry once its
    /// last subscriber leaves, and `publish` drops senders whose receiver has
    /// closed, so a key in `channel_subs` implies a live subscriber. The
    /// `is_empty` guard covers the one window where it does not: a connection
    /// that died between the last publish and its close handler.
    pub(crate) fn active_channels(&self) -> impl Iterator<Item = &String> {
        self.channel_subs
            .iter()
            .filter(|(_, subs)| !subs.is_empty())
            .map(|(channel, _)| channel)
    }

    /// Subscribers to one exact channel, for `PUBSUB NUMSUB`. Pattern
    /// subscribers are deliberately not counted, matching Redis: a `PSUBSCRIBE`
    /// is reported by `NUMPAT`, and counting it here would double-count a
    /// client that holds both.
    pub(crate) fn subscriber_count(&self, channel: &str) -> i64 {
        self.channel_subs
            .get(channel)
            .map(|subs| subs.len() as i64)
            .unwrap_or(0)
    }

    /// Distinct patterns under subscription, for `PUBSUB NUMPAT`. Distinct is
    /// the Redis definition: two clients on `news.*` are one pattern, not two.
    pub(crate) fn pattern_count(&self) -> i64 {
        let mut seen: Vec<&str> = self
            .pattern_subs
            .iter()
            .map(|(p, _, _)| p.as_str())
            .collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len() as i64
    }

    /// Deliver to all matching subscribers; returns the count delivered.
    pub(crate) fn publish(&mut self, channel: &str, message: &[u8]) -> i64 {
        let mut count = 0i64;

        if let std::collections::hash_map::Entry::Occupied(mut e) =
            self.channel_subs.entry(channel.to_string())
        {
            let subs = e.get_mut();
            subs.retain(|(_, tx)| {
                let ok = tx
                    .send(PubSubMsg::Message {
                        channel: channel.to_string(),
                        message: message.to_vec(),
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
            .filter(|(p, _, _)| core_engine::store::glob_match(p, channel))
            .map(|(p, _, tx)| (p.clone(), tx.clone()))
            .collect();
        for (pattern, tx) in pattern_txs {
            if tx
                .send(PubSubMsg::PMessage {
                    pattern,
                    channel: channel.to_string(),
                    message: message.to_vec(),
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

pub(crate) type SharedPubSub = Arc<tokio::sync::Mutex<PubSubHub>>;

// ── observable keys ───────────────────────────────────────────────────────────

pub(crate) fn encode_pubsub_msg(msg: PubSubMsg, protover: u8) -> Vec<u8> {
    let frame = |parts: Vec<Value>| {
        if protover >= 3 {
            Value::Push(parts)
        } else {
            Value::Array(Some(parts))
        }
    };
    match msg {
        PubSubMsg::Message { channel, message } => frame(vec![
            Value::BulkString(Some(b"message".to_vec())),
            Value::BulkString(Some(channel.into_bytes())),
            Value::BulkString(Some(message)),
        ])
        .serialize(),
        PubSubMsg::PMessage {
            pattern,
            channel,
            message,
        } => frame(vec![
            Value::BulkString(Some(b"pmessage".to_vec())),
            Value::BulkString(Some(pattern.into_bytes())),
            Value::BulkString(Some(channel.into_bytes())),
            Value::BulkString(Some(message)),
        ])
        .serialize(),
    }
}

pub(crate) fn resp_subscribe_ack(kind: &str, channel: &str, count: usize) -> Vec<u8> {
    Value::Array(Some(vec![
        Value::BulkString(Some(kind.as_bytes().to_vec())),
        Value::BulkString(Some(channel.as_bytes().to_vec())),
        Value::Integer(count as i64),
    ]))
    .serialize()
}
