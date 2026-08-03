//! Sync scopes: signed tokens restricting a WebSocket connection to a set of
//! key patterns, and the per-command classification they are checked against.

use crate::*;

/// One mutation pushed towards WebSocket peers: the RESP push frame plus the
/// keys it touches, so each connection can filter against its sync scopes
/// without re-parsing the frame. Wrapped in `Arc` — the broadcast channel
/// clones the payload once per receiver, so a clone is a refcount bump.
pub(crate) struct SyncPush {
    pub(crate) origin: u64,
    pub(crate) keys: Vec<String>,
    pub(crate) resp: Vec<u8>,
}

pub(crate) type SyncMsg = Arc<SyncPush>;

/// True when a mutation touching `keys` is visible to a connection whose sync
/// scopes are `scopes`. A mutation with no keys (FLUSHDB) affects every scope.
pub(crate) fn scopes_match(scopes: &[String], keys: &[String]) -> bool {
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
pub(crate) fn verify_sync_token(secret: &str, token: &str) -> Result<Vec<String>, &'static str> {
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
    // Token patterns reach `glob_match` without passing through the command
    // parser, so the cap `check_pattern` applies to `KEYS`/`SCAN`/`PSUBSCRIBE`
    // has to be repeated here. These are matched once per key per write, so an
    // over-long one is the most expensive place to put a pattern — and a
    // compromised or careless minting service should not be able to.
    if patterns
        .iter()
        .any(|p| p.len() > core_engine::store::MAX_PATTERN_BYTES)
    {
        return Err("token grants an over-long pattern");
    }
    Ok(patterns)
}

/// What a command touches, for scope enforcement on token-scoped WebSocket
/// connections.
#[derive(Debug)]
pub(crate) enum CommandScope {
    /// No key access (PING, AUTH, MULTI, SYNC, pub/sub) — always allowed.
    KeyLess,
    /// Touches exactly these keys — every one must match a scope pattern.
    Keys(Vec<String>),
    /// Keyspace-wide or administrative — denied on scoped connections.
    Admin,
}

pub(crate) fn command_scope(cmd: &Command) -> CommandScope {
    match cmd {
        Command::Ping(_)
        | Command::Auth(_)
        | Command::Hello(_)
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
        // QUIT and CLIENT describe the connection itself, which a scoped
        // connection is entitled to know about; COMMAND describes the server's
        // vocabulary, which is public.
        | Command::Quit
        | Command::Client(_)
        | Command::CommandQuery(_)
        // CLUSTER and MODULE answer the same sentence to everyone — "not a
        // cluster", "no modules" — and describe no state a scope could protect.
        | Command::Cluster(_)
        | Command::Module(_)
        // Every MEMORY subcommand other than USAGE is refused outright, so
        // there is nothing here to scope either. USAGE reads a key and is
        // classified with the key commands below.
        | Command::Memory(_)
        | Command::Unknown(_) => CommandScope::KeyLess,

        Command::Keys(_)
        | Command::Scan(_, _, _)
        | Command::DbSize
        | Command::FlushDb
        | Command::Save
        | Command::BgSave
        | Command::LastSave
        // INFO reports server-wide state — uptime, client counts, keyspace
        // size, replication topology. A connection scoped to a handful of keys
        // has no business reading it.
        | Command::Info(_)
        // CONFIG reports server-wide limits and whether auth is on. Same
        // reasoning as INFO: not for a connection scoped to a few keys.
        | Command::Config(_)
        // PUBSUB enumerates every channel every other client is subscribed to.
        // A scoped connection can already SUBSCRIBE to any channel it can name
        // — channels are outside the scope system entirely — but naming and
        // listing are different powers, the same way GET is scoped and KEYS is
        // Admin. NUMSUB and NUMPAT ride along rather than splitting the family
        // across two scopes for one subcommand's worth of difference.
        | Command::PubSub(_)
        | Command::ReplicaOfNoOne => CommandScope::Admin,

        Command::ESet(k, _)
        | Command::Set(k, _, _)
        | Command::Get(k)
        | Command::Append(k, _)
        | Command::Strlen(k)
        | Command::GetRange(k, _, _)
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
        | Command::MemoryUsage(k)
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
        | Command::HScan(k, _)
        | Command::SScan(k, _)
        | Command::ZScan(k, _)
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
pub(crate) fn handle_sync_command(
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
