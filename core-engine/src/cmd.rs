use crate::resp::Value;

// ── SET options ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum SetExpiry {
    Ex(u64),
    Px(u64),
    Exat(u64),
    Pxat(u64),
    KeepTtl,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SetCondition {
    Nx,
    Xx,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SetOptions {
    pub expiry: Option<SetExpiry>,
    pub condition: Option<SetCondition>,
    pub get: bool,
}

// ── ZADD options ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ZAddCondition {
    Nx,
    Xx,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ZAddOptions {
    pub condition: Option<ZAddCondition>,
    pub gt: bool,
    pub lt: bool,
    pub ch: bool,
    pub incr: bool,
}

// ── Command enum ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Ping(Option<String>),
    Auth(String),
    // ── Strings ──────────────────────────────────────────────────────────────
    Set(String, String, SetOptions),
    Get(String),
    Del(Vec<String>),
    Unlink(Vec<String>),
    Append(String, String),
    Strlen(String),
    GetSet(String, String),
    MGet(Vec<String>),
    MSet(Vec<(String, String)>),
    SetNx(String, String),
    SetEx(String, u64, String),
    PSetEx(String, u64, String),
    Incr(String),
    Decr(String),
    IncrBy(String, i64),
    DecrBy(String, i64),
    // ── Expiry ────────────────────────────────────────────────────────────────
    Expire(String, u64),
    PExpire(String, u64),
    ExpireAt(String, u64),
    PExpireAt(String, u64),
    Ttl(String),
    PTtl(String),
    Persist(String),
    // ── Keys ─────────────────────────────────────────────────────────────────
    Exists(Vec<String>),
    Keys(String),
    Scan(u64, Option<String>, Option<usize>),
    DbSize,
    FlushDb,
    Rename(String, String),
    Type(String),
    // ── Hash ─────────────────────────────────────────────────────────────────
    HSet(String, Vec<(String, String)>),
    HGet(String, String),
    HGetAll(String),
    HDel(String, Vec<String>),
    HKeys(String),
    HVals(String),
    HLen(String),
    HIncrBy(String, String, i64),
    HIncrByFloat(String, String, f64),
    HExists(String, String),
    HSetNx(String, String, String),
    HMGet(String, Vec<String>),
    // ── List ─────────────────────────────────────────────────────────────────
    LPush(String, Vec<String>),
    RPush(String, Vec<String>),
    LPushX(String, Vec<String>),
    RPushX(String, Vec<String>),
    LPop(String, Option<u64>),
    RPop(String, Option<u64>),
    LRange(String, i64, i64),
    LLen(String),
    LIndex(String, i64),
    LSet(String, i64, String),
    LRem(String, i64, String),
    LTrim(String, i64, i64),
    // ── Set ──────────────────────────────────────────────────────────────────
    SAdd(String, Vec<String>),
    SMembers(String),
    SRem(String, Vec<String>),
    SCard(String),
    SIsMember(String, String),
    SMIsMember(String, Vec<String>),
    SInter(Vec<String>),
    SInterStore(String, Vec<String>),
    SUnion(Vec<String>),
    SUnionStore(String, Vec<String>),
    SDiff(Vec<String>),
    SDiffStore(String, Vec<String>),
    SPop(String, Option<u64>),
    SRandMember(String, Option<i64>),
    SMove(String, String, String),
    // ── Sorted Set ───────────────────────────────────────────────────────────
    ZAdd(String, ZAddOptions, Vec<(f64, String)>),
    ZRange(String, i64, i64, bool),
    ZRevRange(String, i64, i64, bool),
    ZRangeByScore(String, String, String, bool, Option<(i64, i64)>),
    ZRevRangeByScore(String, String, String, bool, Option<(i64, i64)>),
    ZScore(String, String),
    ZMScore(String, Vec<String>),
    ZRank(String, String),
    ZRevRank(String, String),
    ZRem(String, Vec<String>),
    ZCard(String),
    ZIncrBy(String, f64, String),
    ZCount(String, String, String),
    // ── Rate limiting ─────────────────────────────────────────────────────────
    /// RLSET key limit window_secs — configure a sliding-window rate limiter.
    RlSet(String, u64, u64),
    /// RLCHECK key [limit window_secs] — record an attempt and report
    /// [allowed, remaining, retry_after_ms]. The optional limit/window pair
    /// configures the limiter on first use (upsert), for per-IP/per-user keys
    /// where a separate RLSET round-trip per key is impractical.
    RlCheck(String, Option<(u64, u64)>),
    // ── Transactions ─────────────────────────────────────────────────────────
    Multi,
    Exec,
    Discard,
    // ── Pub/Sub ───────────────────────────────────────────────────────────────
    Subscribe(Vec<String>),
    Unsubscribe(Vec<String>),
    PSubscribe(Vec<String>),
    PUnsubscribe(Vec<String>),
    Publish(String, String),
    // ── Observable keys ───────────────────────────────────────────────────────
    Watch(Vec<String>),
    Unwatch(Vec<String>),
    // ── Persistence ───────────────────────────────────────────────────────────
    Save,
    BgSave,
    LastSave,
    // ── Replication ───────────────────────────────────────────────────────────
    ReplicaOfNoOne,
    Unknown(String),
}

impl Command {
    pub fn from_value(value: Value) -> Result<Command, String> {
        match value {
            Value::Array(Some(arr)) => {
                if arr.is_empty() {
                    return Err("Empty command".to_string());
                }
                let cmd_name = match &arr[0] {
                    Value::BulkString(Some(data)) => String::from_utf8_lossy(data).to_uppercase(),
                    Value::SimpleString(s) => s.to_uppercase(),
                    _ => return Err("Invalid command name type".to_string()),
                };

                macro_rules! need {
                    ($n:expr) => {
                        if arr.len() < $n {
                            return Err(format!(
                                "ERR wrong number of arguments for '{}' command",
                                cmd_name.to_lowercase()
                            ));
                        }
                    };
                }

                match cmd_name.as_str() {
                    // ── Core ─────────────────────────────────────────────────
                    "PING" => {
                        let msg = if arr.len() > 1 {
                            extract_string(&arr[1])
                        } else {
                            None
                        };
                        Ok(Command::Ping(msg))
                    }
                    "AUTH" => {
                        need!(2);
                        Ok(Command::Auth(extract_string(&arr[1]).unwrap_or_default()))
                    }

                    // ── Strings ───────────────────────────────────────────────
                    "SET" => {
                        need!(3);
                        let key = extract_key(&arr[1])?;
                        let val = extract_string(&arr[2]).unwrap_or_default();
                        let mut opts = SetOptions::default();
                        let mut i = 3usize;
                        while i < arr.len() {
                            let flag = extract_string(&arr[i]).unwrap_or_default().to_uppercase();
                            match flag.as_str() {
                                "EX" => {
                                    i += 1;
                                    if i >= arr.len() {
                                        return Err("ERR syntax error".to_string());
                                    }
                                    let n = extract_int(&arr[i])?;
                                    if n <= 0 {
                                        return Err(
                                            "ERR invalid expire time in 'set' command".to_string()
                                        );
                                    }
                                    opts.expiry = Some(SetExpiry::Ex(n as u64));
                                }
                                "PX" => {
                                    i += 1;
                                    if i >= arr.len() {
                                        return Err("ERR syntax error".to_string());
                                    }
                                    let n = extract_int(&arr[i])?;
                                    if n <= 0 {
                                        return Err(
                                            "ERR invalid expire time in 'set' command".to_string()
                                        );
                                    }
                                    opts.expiry = Some(SetExpiry::Px(n as u64));
                                }
                                "EXAT" => {
                                    i += 1;
                                    if i >= arr.len() {
                                        return Err("ERR syntax error".to_string());
                                    }
                                    let n = extract_int(&arr[i])?;
                                    if n <= 0 {
                                        return Err(
                                            "ERR invalid expire time in 'set' command".to_string()
                                        );
                                    }
                                    opts.expiry = Some(SetExpiry::Exat(n as u64));
                                }
                                "PXAT" => {
                                    i += 1;
                                    if i >= arr.len() {
                                        return Err("ERR syntax error".to_string());
                                    }
                                    let n = extract_int(&arr[i])?;
                                    if n <= 0 {
                                        return Err(
                                            "ERR invalid expire time in 'set' command".to_string()
                                        );
                                    }
                                    opts.expiry = Some(SetExpiry::Pxat(n as u64));
                                }
                                "KEEPTTL" => {
                                    opts.expiry = Some(SetExpiry::KeepTtl);
                                }
                                "NX" => opts.condition = Some(SetCondition::Nx),
                                "XX" => opts.condition = Some(SetCondition::Xx),
                                "GET" => opts.get = true,
                                _ => return Err("ERR syntax error".to_string()),
                            }
                            i += 1;
                        }
                        Ok(Command::Set(key, val, opts))
                    }
                    "GET" => {
                        need!(2);
                        Ok(Command::Get(extract_key(&arr[1])?))
                    }
                    "DEL" => {
                        need!(2);
                        Ok(Command::Del(extract_keys(&arr[1..])?))
                    }
                    "UNLINK" => {
                        need!(2);
                        Ok(Command::Unlink(extract_keys(&arr[1..])?))
                    }
                    "APPEND" => {
                        need!(3);
                        Ok(Command::Append(
                            extract_key(&arr[1])?,
                            extract_string(&arr[2]).unwrap_or_default(),
                        ))
                    }
                    "STRLEN" => {
                        need!(2);
                        Ok(Command::Strlen(extract_key(&arr[1])?))
                    }
                    "GETSET" => {
                        need!(3);
                        Ok(Command::GetSet(
                            extract_key(&arr[1])?,
                            extract_string(&arr[2]).unwrap_or_default(),
                        ))
                    }
                    "MGET" => {
                        need!(2);
                        Ok(Command::MGet(extract_keys(&arr[1..])?))
                    }
                    "MSET" => {
                        if arr.len() < 3 || (arr.len() - 1) % 2 != 0 {
                            return Err(
                                "ERR wrong number of arguments for 'mset' command".to_string()
                            );
                        }
                        let pairs = arr[1..]
                            .chunks(2)
                            .map(|c| {
                                Ok((
                                    extract_key(&c[0])?,
                                    extract_string(&c[1]).unwrap_or_default(),
                                ))
                            })
                            .collect::<Result<Vec<_>, String>>()?;
                        Ok(Command::MSet(pairs))
                    }
                    "SETNX" => {
                        need!(3);
                        Ok(Command::SetNx(
                            extract_key(&arr[1])?,
                            extract_string(&arr[2]).unwrap_or_default(),
                        ))
                    }
                    "SETEX" => {
                        need!(4);
                        let secs = extract_int(&arr[2])?;
                        if secs <= 0 {
                            return Err("ERR invalid expire time in 'setex' command".to_string());
                        }
                        Ok(Command::SetEx(
                            extract_key(&arr[1])?,
                            secs as u64,
                            extract_string(&arr[3]).unwrap_or_default(),
                        ))
                    }
                    "PSETEX" => {
                        need!(4);
                        let ms = extract_int(&arr[2])?;
                        if ms <= 0 {
                            return Err("ERR invalid expire time in 'psetex' command".to_string());
                        }
                        Ok(Command::PSetEx(
                            extract_key(&arr[1])?,
                            ms as u64,
                            extract_string(&arr[3]).unwrap_or_default(),
                        ))
                    }
                    "INCR" => {
                        need!(2);
                        Ok(Command::Incr(extract_key(&arr[1])?))
                    }
                    "DECR" => {
                        need!(2);
                        Ok(Command::Decr(extract_key(&arr[1])?))
                    }
                    "INCRBY" => {
                        need!(3);
                        Ok(Command::IncrBy(
                            extract_key(&arr[1])?,
                            extract_int(&arr[2])?,
                        ))
                    }
                    "DECRBY" => {
                        need!(3);
                        Ok(Command::DecrBy(
                            extract_key(&arr[1])?,
                            extract_int(&arr[2])?,
                        ))
                    }

                    // ── Expiry ─────────────────────────────────────────────────
                    "EXPIRE" => {
                        need!(3);
                        let secs = extract_int(&arr[2])?;
                        if secs < 0 {
                            return Err("ERR invalid expire time in 'expire' command".to_string());
                        }
                        Ok(Command::Expire(
                            extract_string(&arr[1]).unwrap_or_default(),
                            secs as u64,
                        ))
                    }
                    "PEXPIRE" => {
                        need!(3);
                        let ms = extract_int(&arr[2])?;
                        if ms < 0 {
                            return Err("ERR invalid expire time in 'pexpire' command".to_string());
                        }
                        Ok(Command::PExpire(
                            extract_string(&arr[1]).unwrap_or_default(),
                            ms as u64,
                        ))
                    }
                    "EXPIREAT" => {
                        need!(3);
                        let ts = extract_int(&arr[2])?;
                        if ts < 0 {
                            return Err("ERR invalid expire time in 'expireat' command".to_string());
                        }
                        Ok(Command::ExpireAt(
                            extract_string(&arr[1]).unwrap_or_default(),
                            ts as u64,
                        ))
                    }
                    "PEXPIREAT" => {
                        need!(3);
                        let ts = extract_int(&arr[2])?;
                        if ts < 0 {
                            return Err(
                                "ERR invalid expire time in 'pexpireat' command".to_string()
                            );
                        }
                        Ok(Command::PExpireAt(
                            extract_string(&arr[1]).unwrap_or_default(),
                            ts as u64,
                        ))
                    }
                    "TTL" => {
                        need!(2);
                        Ok(Command::Ttl(extract_string(&arr[1]).unwrap_or_default()))
                    }
                    "PTTL" => {
                        need!(2);
                        Ok(Command::PTtl(extract_string(&arr[1]).unwrap_or_default()))
                    }
                    "PERSIST" => {
                        need!(2);
                        Ok(Command::Persist(
                            extract_string(&arr[1]).unwrap_or_default(),
                        ))
                    }

                    // ── Keys ───────────────────────────────────────────────────
                    "EXISTS" => {
                        need!(2);
                        Ok(Command::Exists(extract_keys(&arr[1..])?))
                    }
                    "KEYS" => {
                        need!(2);
                        Ok(Command::Keys(extract_string(&arr[1]).unwrap_or_default()))
                    }
                    "SCAN" => {
                        need!(2);
                        let cursor = extract_string(&arr[1])
                            .unwrap_or_default()
                            .parse::<u64>()
                            .map_err(|_| {
                                "ERR value is not an integer or out of range".to_string()
                            })?;
                        let mut pattern = None;
                        let mut count = None;
                        let mut i = 2usize;
                        while i < arr.len() {
                            let opt = extract_string(&arr[i]).unwrap_or_default().to_uppercase();
                            match opt.as_str() {
                                "MATCH" => {
                                    i += 1;
                                    if i >= arr.len() {
                                        return Err("ERR syntax error".to_string());
                                    }
                                    pattern = extract_string(&arr[i]);
                                }
                                "COUNT" => {
                                    i += 1;
                                    if i >= arr.len() {
                                        return Err("ERR syntax error".to_string());
                                    }
                                    count = Some(extract_int(&arr[i])? as usize);
                                }
                                _ => return Err("ERR syntax error".to_string()),
                            }
                            i += 1;
                        }
                        Ok(Command::Scan(cursor, pattern, count))
                    }
                    "DBSIZE" => Ok(Command::DbSize),
                    "FLUSHDB" => Ok(Command::FlushDb),
                    "RENAME" => {
                        need!(3);
                        Ok(Command::Rename(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_string(&arr[2]).unwrap_or_default(),
                        ))
                    }
                    "TYPE" => {
                        need!(2);
                        Ok(Command::Type(extract_string(&arr[1]).unwrap_or_default()))
                    }

                    // ── Rate limiting ──────────────────────────────────────────
                    "RLSET" => {
                        need!(4);
                        let key = extract_key(&arr[1])?;
                        let (limit, window) = parse_rl_config(&arr[2], &arr[3], "rlset")?;
                        Ok(Command::RlSet(key, limit, window))
                    }
                    "RLCHECK" => {
                        need!(2);
                        let key = extract_key(&arr[1])?;
                        let config = match arr.len() {
                            2 => None,
                            4 => Some(parse_rl_config(&arr[2], &arr[3], "rlcheck")?),
                            _ => {
                                return Err("ERR wrong number of arguments for 'rlcheck' command"
                                    .to_string());
                            }
                        };
                        Ok(Command::RlCheck(key, config))
                    }

                    // ── Hash ───────────────────────────────────────────────────
                    "HSET" | "HMSET" => {
                        if arr.len() < 4 || (arr.len() - 2) % 2 != 0 {
                            return Err(format!(
                                "ERR wrong number of arguments for '{}' command",
                                cmd_name.to_lowercase()
                            ));
                        }
                        let key = extract_key(&arr[1])?;
                        let pairs = arr[2..]
                            .chunks(2)
                            .map(|c| {
                                (
                                    extract_string(&c[0]).unwrap_or_default(),
                                    extract_string(&c[1]).unwrap_or_default(),
                                )
                            })
                            .collect();
                        Ok(Command::HSet(key, pairs))
                    }
                    "HGET" => {
                        need!(3);
                        Ok(Command::HGet(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_string(&arr[2]).unwrap_or_default(),
                        ))
                    }
                    "HGETALL" => {
                        need!(2);
                        Ok(Command::HGetAll(
                            extract_string(&arr[1]).unwrap_or_default(),
                        ))
                    }
                    "HDEL" => {
                        need!(3);
                        let key = extract_key(&arr[1])?;
                        let fields = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::HDel(key, fields))
                    }
                    "HKEYS" => {
                        need!(2);
                        Ok(Command::HKeys(extract_string(&arr[1]).unwrap_or_default()))
                    }
                    "HVALS" => {
                        need!(2);
                        Ok(Command::HVals(extract_string(&arr[1]).unwrap_or_default()))
                    }
                    "HLEN" => {
                        need!(2);
                        Ok(Command::HLen(extract_string(&arr[1]).unwrap_or_default()))
                    }
                    "HINCRBY" => {
                        need!(4);
                        Ok(Command::HIncrBy(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_string(&arr[2]).unwrap_or_default(),
                            extract_int(&arr[3])?,
                        ))
                    }
                    "HINCRBYFLOAT" => {
                        need!(4);
                        let inc = extract_float(&arr[3])?;
                        Ok(Command::HIncrByFloat(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_string(&arr[2]).unwrap_or_default(),
                            inc,
                        ))
                    }
                    "HEXISTS" => {
                        need!(3);
                        Ok(Command::HExists(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_string(&arr[2]).unwrap_or_default(),
                        ))
                    }
                    "HSETNX" => {
                        need!(4);
                        Ok(Command::HSetNx(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_string(&arr[2]).unwrap_or_default(),
                            extract_string(&arr[3]).unwrap_or_default(),
                        ))
                    }
                    "HMGET" => {
                        need!(3);
                        let key = extract_key(&arr[1])?;
                        let fields = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::HMGet(key, fields))
                    }

                    // ── List ───────────────────────────────────────────────────
                    "LPUSH" => {
                        need!(3);
                        let key = extract_key(&arr[1])?;
                        let vals = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::LPush(key, vals))
                    }
                    "RPUSH" => {
                        need!(3);
                        let key = extract_key(&arr[1])?;
                        let vals = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::RPush(key, vals))
                    }
                    "LPUSHX" => {
                        need!(3);
                        let key = extract_key(&arr[1])?;
                        let vals = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::LPushX(key, vals))
                    }
                    "RPUSHX" => {
                        need!(3);
                        let key = extract_key(&arr[1])?;
                        let vals = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::RPushX(key, vals))
                    }
                    "LPOP" => {
                        need!(2);
                        let key = extract_key(&arr[1])?;
                        let count = if arr.len() > 2 {
                            Some(extract_int(&arr[2])? as u64)
                        } else {
                            None
                        };
                        Ok(Command::LPop(key, count))
                    }
                    "RPOP" => {
                        need!(2);
                        let key = extract_key(&arr[1])?;
                        let count = if arr.len() > 2 {
                            Some(extract_int(&arr[2])? as u64)
                        } else {
                            None
                        };
                        Ok(Command::RPop(key, count))
                    }
                    "LRANGE" => {
                        need!(4);
                        Ok(Command::LRange(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_int(&arr[2])?,
                            extract_int(&arr[3])?,
                        ))
                    }
                    "LLEN" => {
                        need!(2);
                        Ok(Command::LLen(extract_string(&arr[1]).unwrap_or_default()))
                    }
                    "LINDEX" => {
                        need!(3);
                        Ok(Command::LIndex(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_int(&arr[2])?,
                        ))
                    }
                    "LSET" => {
                        need!(4);
                        Ok(Command::LSet(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_int(&arr[2])?,
                            extract_string(&arr[3]).unwrap_or_default(),
                        ))
                    }
                    "LREM" => {
                        need!(4);
                        Ok(Command::LRem(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_int(&arr[2])?,
                            extract_string(&arr[3]).unwrap_or_default(),
                        ))
                    }
                    "LTRIM" => {
                        need!(4);
                        Ok(Command::LTrim(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_int(&arr[2])?,
                            extract_int(&arr[3])?,
                        ))
                    }

                    // ── Set ────────────────────────────────────────────────────
                    "SADD" => {
                        need!(3);
                        let key = extract_key(&arr[1])?;
                        let members = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::SAdd(key, members))
                    }
                    "SMEMBERS" => {
                        need!(2);
                        Ok(Command::SMembers(
                            extract_string(&arr[1]).unwrap_or_default(),
                        ))
                    }
                    "SREM" => {
                        need!(3);
                        let key = extract_key(&arr[1])?;
                        let members = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::SRem(key, members))
                    }
                    "SCARD" => {
                        need!(2);
                        Ok(Command::SCard(extract_string(&arr[1]).unwrap_or_default()))
                    }
                    "SISMEMBER" => {
                        need!(3);
                        Ok(Command::SIsMember(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_string(&arr[2]).unwrap_or_default(),
                        ))
                    }
                    "SMISMEMBER" => {
                        need!(3);
                        let key = extract_key(&arr[1])?;
                        let members = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::SMIsMember(key, members))
                    }
                    "SINTER" => {
                        need!(2);
                        Ok(Command::SInter(
                            arr[1..].iter().filter_map(extract_string).collect(),
                        ))
                    }
                    "SINTERSTORE" => {
                        need!(3);
                        let dst = extract_string(&arr[1]).unwrap_or_default();
                        let keys = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::SInterStore(dst, keys))
                    }
                    "SUNION" => {
                        need!(2);
                        Ok(Command::SUnion(
                            arr[1..].iter().filter_map(extract_string).collect(),
                        ))
                    }
                    "SUNIONSTORE" => {
                        need!(3);
                        let dst = extract_string(&arr[1]).unwrap_or_default();
                        let keys = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::SUnionStore(dst, keys))
                    }
                    "SDIFF" => {
                        need!(2);
                        Ok(Command::SDiff(
                            arr[1..].iter().filter_map(extract_string).collect(),
                        ))
                    }
                    "SDIFFSTORE" => {
                        need!(3);
                        let dst = extract_string(&arr[1]).unwrap_or_default();
                        let keys = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::SDiffStore(dst, keys))
                    }
                    "SPOP" => {
                        need!(2);
                        let key = extract_key(&arr[1])?;
                        let count = if arr.len() > 2 {
                            Some(extract_int(&arr[2])? as u64)
                        } else {
                            None
                        };
                        Ok(Command::SPop(key, count))
                    }
                    "SRANDMEMBER" => {
                        need!(2);
                        let key = extract_key(&arr[1])?;
                        let count = if arr.len() > 2 {
                            Some(extract_int(&arr[2])?)
                        } else {
                            None
                        };
                        Ok(Command::SRandMember(key, count))
                    }
                    "SMOVE" => {
                        need!(4);
                        Ok(Command::SMove(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_string(&arr[2]).unwrap_or_default(),
                            extract_string(&arr[3]).unwrap_or_default(),
                        ))
                    }

                    // ── Sorted Set ─────────────────────────────────────────────
                    "ZADD" => {
                        need!(4);
                        let key = extract_key(&arr[1])?;
                        let mut opts = ZAddOptions::default();
                        let mut i = 2usize;

                        // Parse leading options (until we hit a parseable float)
                        while i < arr.len() {
                            let tok = extract_string(&arr[i]).unwrap_or_default().to_uppercase();
                            match tok.as_str() {
                                "NX" => {
                                    opts.condition = Some(ZAddCondition::Nx);
                                    i += 1;
                                }
                                "XX" => {
                                    opts.condition = Some(ZAddCondition::Xx);
                                    i += 1;
                                }
                                "GT" => {
                                    opts.gt = true;
                                    i += 1;
                                }
                                "LT" => {
                                    opts.lt = true;
                                    i += 1;
                                }
                                "CH" => {
                                    opts.ch = true;
                                    i += 1;
                                }
                                "INCR" => {
                                    opts.incr = true;
                                    i += 1;
                                }
                                _ => break,
                            }
                        }

                        if (arr.len() - i) < 2 || !(arr.len() - i).is_multiple_of(2) {
                            return Err("ERR syntax error".to_string());
                        }

                        let mut pairs = Vec::new();
                        while i < arr.len() {
                            let score = extract_float(&arr[i])?;
                            let member = extract_string(&arr[i + 1]).unwrap_or_default();
                            pairs.push((score, member));
                            i += 2;
                        }

                        if opts.gt && opts.lt {
                            return Err(
                                "ERR GT and LT options at the same time are not compatible"
                                    .to_string(),
                            );
                        }
                        if (opts.gt || opts.lt) && opts.condition == Some(ZAddCondition::Nx) {
                            return Err(
                                "ERR GT, LT, and NX options at the same time are not compatible"
                                    .to_string(),
                            );
                        }

                        if opts.incr && pairs.len() != 1 {
                            return Err("ERR INCR option supports a single increment-element pair"
                                .to_string());
                        }

                        Ok(Command::ZAdd(key, opts, pairs))
                    }
                    "ZRANGE" => {
                        need!(4);
                        let key = extract_key(&arr[1])?;
                        let start = extract_int(&arr[2])?;
                        let stop = extract_int(&arr[3])?;
                        let withscores = arr
                            .get(4)
                            .and_then(extract_string)
                            .map(|s| s.to_uppercase() == "WITHSCORES")
                            .unwrap_or(false);
                        Ok(Command::ZRange(key, start, stop, withscores))
                    }
                    "ZREVRANGE" => {
                        need!(4);
                        let key = extract_key(&arr[1])?;
                        let start = extract_int(&arr[2])?;
                        let stop = extract_int(&arr[3])?;
                        let withscores = arr
                            .get(4)
                            .and_then(extract_string)
                            .map(|s| s.to_uppercase() == "WITHSCORES")
                            .unwrap_or(false);
                        Ok(Command::ZRevRange(key, start, stop, withscores))
                    }
                    "ZRANGEBYSCORE" => {
                        need!(4);
                        let key = extract_key(&arr[1])?;
                        let min = extract_string(&arr[2]).unwrap_or_default();
                        let max = extract_string(&arr[3]).unwrap_or_default();
                        let (withscores, limit) = parse_zrange_opts(&arr[4..])?;
                        Ok(Command::ZRangeByScore(key, min, max, withscores, limit))
                    }
                    "ZREVRANGEBYSCORE" => {
                        need!(4);
                        let key = extract_key(&arr[1])?;
                        let max = extract_string(&arr[2]).unwrap_or_default();
                        let min = extract_string(&arr[3]).unwrap_or_default();
                        let (withscores, limit) = parse_zrange_opts(&arr[4..])?;
                        Ok(Command::ZRevRangeByScore(key, max, min, withscores, limit))
                    }
                    "ZSCORE" => {
                        need!(3);
                        Ok(Command::ZScore(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_string(&arr[2]).unwrap_or_default(),
                        ))
                    }
                    "ZMSCORE" => {
                        need!(3);
                        let key = extract_key(&arr[1])?;
                        let members = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::ZMScore(key, members))
                    }
                    "ZRANK" => {
                        need!(3);
                        Ok(Command::ZRank(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_string(&arr[2]).unwrap_or_default(),
                        ))
                    }
                    "ZREVRANK" => {
                        need!(3);
                        Ok(Command::ZRevRank(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_string(&arr[2]).unwrap_or_default(),
                        ))
                    }
                    "ZREM" => {
                        need!(3);
                        let key = extract_key(&arr[1])?;
                        let members = arr[2..].iter().filter_map(extract_string).collect();
                        Ok(Command::ZRem(key, members))
                    }
                    "ZCARD" => {
                        need!(2);
                        Ok(Command::ZCard(extract_string(&arr[1]).unwrap_or_default()))
                    }
                    "ZINCRBY" => {
                        need!(4);
                        let inc = extract_float(&arr[2])?;
                        Ok(Command::ZIncrBy(
                            extract_string(&arr[1]).unwrap_or_default(),
                            inc,
                            extract_string(&arr[3]).unwrap_or_default(),
                        ))
                    }
                    "ZCOUNT" => {
                        need!(4);
                        Ok(Command::ZCount(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_string(&arr[2]).unwrap_or_default(),
                            extract_string(&arr[3]).unwrap_or_default(),
                        ))
                    }

                    // ── Transactions ─────────────────────────────────────
                    "MULTI" => Ok(Command::Multi),
                    "EXEC" => Ok(Command::Exec),
                    "DISCARD" => Ok(Command::Discard),

                    // ── Pub/Sub ───────────────────────────────────────────
                    "SUBSCRIBE" => {
                        need!(2);
                        Ok(Command::Subscribe(
                            arr[1..].iter().filter_map(extract_string).collect(),
                        ))
                    }
                    "UNSUBSCRIBE" => Ok(Command::Unsubscribe(
                        arr[1..].iter().filter_map(extract_string).collect(),
                    )),
                    "PSUBSCRIBE" => {
                        need!(2);
                        Ok(Command::PSubscribe(
                            arr[1..].iter().filter_map(extract_string).collect(),
                        ))
                    }
                    "PUNSUBSCRIBE" => Ok(Command::PUnsubscribe(
                        arr[1..].iter().filter_map(extract_string).collect(),
                    )),
                    "PUBLISH" => {
                        need!(3);
                        Ok(Command::Publish(
                            extract_string(&arr[1]).unwrap_or_default(),
                            extract_string(&arr[2]).unwrap_or_default(),
                        ))
                    }

                    // ── Observable keys ───────────────────────────────────────
                    "WATCH" => {
                        need!(2);
                        Ok(Command::Watch(
                            arr[1..].iter().filter_map(extract_string).collect(),
                        ))
                    }
                    "UNWATCH" => Ok(Command::Unwatch(
                        arr[1..].iter().filter_map(extract_string).collect(),
                    )),

                    // ── Persistence ───────────────────────────────────────────
                    "SAVE" => Ok(Command::Save),
                    "BGSAVE" => Ok(Command::BgSave),
                    "LASTSAVE" => Ok(Command::LastSave),

                    // ── Replication ───────────────────────────────────────────
                    "REPLICAOF" => {
                        need!(3);
                        let arg1 = extract_string(&arr[1]).unwrap_or_default().to_uppercase();
                        let arg2 = extract_string(&arr[2]).unwrap_or_default().to_uppercase();
                        if arg1 == "NO" && arg2 == "ONE" {
                            Ok(Command::ReplicaOfNoOne)
                        } else {
                            Err("ERR REPLICAOF supports only 'REPLICAOF NO ONE' at runtime"
                                .to_string())
                        }
                    }

                    _ => Ok(Command::Unknown(cmd_name.to_owned())),
                }
            }
            _ => Err("Commands must be RESP Arrays".to_string()),
        }
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

const MAX_KEY_BYTES: usize = 512 * 1024; // 512 KB

fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("ERR key cannot be empty".to_string());
    }
    if key.len() > MAX_KEY_BYTES {
        return Err(format!(
            "ERR key too large ({} > {} bytes)",
            key.len(),
            MAX_KEY_BYTES
        ));
    }
    Ok(())
}

/// Parse and validate the `limit window_secs` pair shared by RLSET and RLCHECK.
fn parse_rl_config(limit: &Value, window: &Value, cmd: &str) -> Result<(u64, u64), String> {
    let limit = extract_int(limit)?;
    let window = extract_int(window)?;
    if limit <= 0 {
        return Err(format!("ERR limit must be >= 1 in '{}' command", cmd));
    }
    if window <= 0 {
        return Err(format!("ERR window must be >= 1 in '{}' command", cmd));
    }
    Ok((limit as u64, window as u64))
}

fn extract_key(val: &Value) -> Result<String, String> {
    let key = extract_string(val).unwrap_or_default();
    validate_key(&key)?;
    Ok(key)
}

fn extract_keys(vals: &[Value]) -> Result<Vec<String>, String> {
    vals.iter().map(extract_key).collect()
}

fn extract_string(val: &Value) -> Option<String> {
    match val {
        Value::BulkString(Some(data)) => Some(String::from_utf8_lossy(data).into_owned()),
        Value::SimpleString(s) => Some(s.clone()),
        _ => None,
    }
}

fn extract_int(val: &Value) -> Result<i64, String> {
    match val {
        Value::BulkString(Some(data)) => String::from_utf8_lossy(data)
            .parse::<i64>()
            .map_err(|_| "ERR value is not an integer or out of range".to_string()),
        Value::SimpleString(s) => s
            .parse::<i64>()
            .map_err(|_| "ERR value is not an integer or out of range".to_string()),
        Value::Integer(i) => Ok(*i),
        _ => Err("ERR value is not an integer or out of range".to_string()),
    }
}

fn extract_float(val: &Value) -> Result<f64, String> {
    match val {
        Value::BulkString(Some(data)) => {
            let s = String::from_utf8_lossy(data);
            if s == "inf" || s == "+inf" {
                Ok(f64::INFINITY)
            } else if s == "-inf" {
                Ok(f64::NEG_INFINITY)
            } else {
                s.parse::<f64>()
                    .map_err(|_| "ERR value is not a valid float".to_string())
            }
        }
        Value::SimpleString(s) => s
            .parse::<f64>()
            .map_err(|_| "ERR value is not a valid float".to_string()),
        Value::Integer(i) => Ok(*i as f64),
        _ => Err("ERR value is not a valid float".to_string()),
    }
}

/// Parse `[WITHSCORES] [LIMIT offset count]` options for ZRANGEBYSCORE / ZREVRANGEBYSCORE.
fn parse_zrange_opts(tokens: &[Value]) -> Result<(bool, Option<(i64, i64)>), String> {
    let mut withscores = false;
    let mut limit = None;
    let mut i = 0usize;
    while i < tokens.len() {
        let opt = extract_string(&tokens[i])
            .unwrap_or_default()
            .to_uppercase();
        match opt.as_str() {
            "WITHSCORES" => {
                withscores = true;
                i += 1;
            }
            "LIMIT" => {
                if i + 2 >= tokens.len() {
                    return Err("ERR syntax error".to_string());
                }
                let offset = extract_int(&tokens[i + 1])?;
                let count = extract_int(&tokens[i + 2])?;
                limit = Some((offset, count));
                i += 3;
            }
            _ => return Err("ERR syntax error".to_string()),
        }
    }
    Ok((withscores, limit))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(s: &str) -> Value {
        Value::BulkString(Some(s.as_bytes().to_vec()))
    }

    fn array(parts: &[&str]) -> Value {
        Value::Array(Some(parts.iter().map(|s| bulk(s)).collect()))
    }

    // ── legacy string/key parsing unchanged ─────────────────────────────────
    #[test]
    fn ping_no_arg() {
        assert_eq!(
            Command::from_value(array(&["PING"])).unwrap(),
            Command::Ping(None)
        );
    }
    #[test]
    fn set_plain() {
        let cmd = Command::from_value(array(&["SET", "k", "v"])).unwrap();
        assert_eq!(
            cmd,
            Command::Set("k".into(), "v".into(), SetOptions::default())
        );
    }
    #[test]
    fn set_ex_nx() {
        let cmd = Command::from_value(array(&["SET", "k", "v", "EX", "10", "NX"])).unwrap();
        assert_eq!(
            cmd,
            Command::Set(
                "k".into(),
                "v".into(),
                SetOptions {
                    expiry: Some(SetExpiry::Ex(10)),
                    condition: Some(SetCondition::Nx),
                    get: false,
                }
            )
        );
    }

    // ── Hash ──────────────────────────────────────────────────────────────────
    #[test]
    fn hset_single() {
        let cmd = Command::from_value(array(&["HSET", "h", "f", "v"])).unwrap();
        assert_eq!(
            cmd,
            Command::HSet("h".into(), vec![("f".into(), "v".into())])
        );
    }
    #[test]
    fn hset_multi() {
        let cmd = Command::from_value(array(&["HSET", "h", "f1", "v1", "f2", "v2"])).unwrap();
        assert_eq!(
            cmd,
            Command::HSet(
                "h".into(),
                vec![("f1".into(), "v1".into()), ("f2".into(), "v2".into())]
            )
        );
    }
    #[test]
    fn hmset_alias() {
        let cmd = Command::from_value(array(&["HMSET", "h", "f", "v"])).unwrap();
        assert!(matches!(cmd, Command::HSet(..)));
    }
    #[test]
    fn hget_ok() {
        assert_eq!(
            Command::from_value(array(&["HGET", "h", "f"])).unwrap(),
            Command::HGet("h".into(), "f".into())
        );
    }
    #[test]
    fn hincrby_ok() {
        assert_eq!(
            Command::from_value(array(&["HINCRBY", "h", "f", "5"])).unwrap(),
            Command::HIncrBy("h".into(), "f".into(), 5)
        );
    }

    // ── List ──────────────────────────────────────────────────────────────────
    #[test]
    fn lpush_multi() {
        let cmd = Command::from_value(array(&["LPUSH", "l", "a", "b", "c"])).unwrap();
        assert_eq!(
            cmd,
            Command::LPush("l".into(), vec!["a".into(), "b".into(), "c".into()])
        );
    }
    #[test]
    fn rpop_with_count() {
        let cmd = Command::from_value(array(&["RPOP", "l", "3"])).unwrap();
        assert_eq!(cmd, Command::RPop("l".into(), Some(3)));
    }
    #[test]
    fn lrange_ok() {
        assert_eq!(
            Command::from_value(array(&["LRANGE", "l", "0", "-1"])).unwrap(),
            Command::LRange("l".into(), 0, -1)
        );
    }

    // ── Set ───────────────────────────────────────────────────────────────────
    #[test]
    fn sadd_multi() {
        let cmd = Command::from_value(array(&["SADD", "s", "a", "b"])).unwrap();
        assert_eq!(cmd, Command::SAdd("s".into(), vec!["a".into(), "b".into()]));
    }
    #[test]
    fn sinter_multi_keys() {
        let cmd = Command::from_value(array(&["SINTER", "s1", "s2", "s3"])).unwrap();
        assert_eq!(
            cmd,
            Command::SInter(vec!["s1".into(), "s2".into(), "s3".into()])
        );
    }
    #[test]
    fn smismember_ok() {
        let cmd = Command::from_value(array(&["SMISMEMBER", "s", "a", "b"])).unwrap();
        assert_eq!(
            cmd,
            Command::SMIsMember("s".into(), vec!["a".into(), "b".into()])
        );
    }

    // ── ZSet ──────────────────────────────────────────────────────────────────
    #[test]
    fn zadd_basic() {
        let cmd = Command::from_value(array(&["ZADD", "z", "1.5", "member"])).unwrap();
        assert_eq!(
            cmd,
            Command::ZAdd(
                "z".into(),
                ZAddOptions::default(),
                vec![(1.5, "member".into())]
            )
        );
    }
    #[test]
    fn zadd_nx_ch() {
        let cmd = Command::from_value(array(&["ZADD", "z", "NX", "CH", "1.0", "m"])).unwrap();
        assert_eq!(
            cmd,
            Command::ZAdd(
                "z".into(),
                ZAddOptions {
                    condition: Some(ZAddCondition::Nx),
                    gt: false,
                    lt: false,
                    ch: true,
                    incr: false
                },
                vec![(1.0, "m".into())]
            )
        );
    }
    #[test]
    fn zadd_incr() {
        let cmd = Command::from_value(array(&["ZADD", "z", "INCR", "2.0", "m"])).unwrap();
        assert!(matches!(
            cmd,
            Command::ZAdd(_, ZAddOptions { incr: true, .. }, _)
        ));
    }
    #[test]
    fn zadd_incr_multiple_pairs_error() {
        let r = Command::from_value(array(&["ZADD", "z", "INCR", "1", "a", "2", "b"]));
        assert!(r.is_err());
    }
    #[test]
    fn zrange_withscores() {
        let cmd = Command::from_value(array(&["ZRANGE", "z", "0", "-1", "WITHSCORES"])).unwrap();
        assert_eq!(cmd, Command::ZRange("z".into(), 0, -1, true));
    }
    #[test]
    fn zrangebyscore_inf() {
        let cmd = Command::from_value(array(&["ZRANGEBYSCORE", "z", "-inf", "+inf"])).unwrap();
        assert_eq!(
            cmd,
            Command::ZRangeByScore("z".into(), "-inf".into(), "+inf".into(), false, None)
        );
    }
    #[test]
    fn zrangebyscore_limit() {
        let cmd = Command::from_value(array(&[
            "ZRANGEBYSCORE",
            "z",
            "0",
            "100",
            "WITHSCORES",
            "LIMIT",
            "0",
            "10",
        ]))
        .unwrap();
        assert_eq!(
            cmd,
            Command::ZRangeByScore("z".into(), "0".into(), "100".into(), true, Some((0, 10)))
        );
    }
    #[test]
    fn unknown_command() {
        assert!(matches!(
            Command::from_value(array(&["BLPOP", "k", "0"])).unwrap(),
            Command::Unknown(_)
        ));
    }
    #[test]
    fn non_array_input() {
        assert!(Command::from_value(Value::SimpleString("PING".into())).is_err());
    }

    // ── Phase 4: Transactions ─────────────────────────────────────────────
    #[test]
    fn multi_exec_discard_parse() {
        assert_eq!(
            Command::from_value(array(&["MULTI"])).unwrap(),
            Command::Multi
        );
        assert_eq!(
            Command::from_value(array(&["EXEC"])).unwrap(),
            Command::Exec
        );
        assert_eq!(
            Command::from_value(array(&["DISCARD"])).unwrap(),
            Command::Discard
        );
    }

    // ── Phase 4: Pub/Sub ──────────────────────────────────────────────────
    #[test]
    fn subscribe_parse() {
        let cmd = Command::from_value(array(&["SUBSCRIBE", "ch1", "ch2"])).unwrap();
        assert_eq!(cmd, Command::Subscribe(vec!["ch1".into(), "ch2".into()]));
    }
    #[test]
    fn subscribe_requires_channel() {
        assert!(Command::from_value(array(&["SUBSCRIBE"])).is_err());
    }
    #[test]
    fn unsubscribe_no_args() {
        let cmd = Command::from_value(array(&["UNSUBSCRIBE"])).unwrap();
        assert_eq!(cmd, Command::Unsubscribe(vec![]));
    }
    #[test]
    fn psubscribe_parse() {
        let cmd = Command::from_value(array(&["PSUBSCRIBE", "news.*"])).unwrap();
        assert_eq!(cmd, Command::PSubscribe(vec!["news.*".into()]));
    }
    #[test]
    fn publish_parse() {
        let cmd = Command::from_value(array(&["PUBLISH", "chan", "hello"])).unwrap();
        assert_eq!(cmd, Command::Publish("chan".into(), "hello".into()));
    }
    #[test]
    fn publish_requires_args() {
        assert!(Command::from_value(array(&["PUBLISH", "chan"])).is_err());
    }

    // ── Strings (parsing) ─────────────────────────────────────────────────────

    #[test]
    fn get_parse() {
        assert_eq!(
            Command::from_value(array(&["GET", "k"])).unwrap(),
            Command::Get("k".into())
        );
    }

    #[test]
    fn del_multi_key_parse() {
        assert_eq!(
            Command::from_value(array(&["DEL", "a", "b", "c"])).unwrap(),
            Command::Del(vec!["a".into(), "b".into(), "c".into()])
        );
    }

    #[test]
    fn unlink_parse() {
        assert_eq!(
            Command::from_value(array(&["UNLINK", "a", "b"])).unwrap(),
            Command::Unlink(vec!["a".into(), "b".into()])
        );
    }

    #[test]
    fn append_parse() {
        assert_eq!(
            Command::from_value(array(&["APPEND", "k", "v"])).unwrap(),
            Command::Append("k".into(), "v".into())
        );
    }

    #[test]
    fn strlen_parse() {
        assert_eq!(
            Command::from_value(array(&["STRLEN", "k"])).unwrap(),
            Command::Strlen("k".into())
        );
    }

    #[test]
    fn getset_parse() {
        assert_eq!(
            Command::from_value(array(&["GETSET", "k", "v"])).unwrap(),
            Command::GetSet("k".into(), "v".into())
        );
    }

    #[test]
    fn mget_parse() {
        assert_eq!(
            Command::from_value(array(&["MGET", "a", "b", "c"])).unwrap(),
            Command::MGet(vec!["a".into(), "b".into(), "c".into()])
        );
    }

    #[test]
    fn mset_parse() {
        assert_eq!(
            Command::from_value(array(&["MSET", "a", "1", "b", "2"])).unwrap(),
            Command::MSet(vec![("a".into(), "1".into()), ("b".into(), "2".into())])
        );
    }

    #[test]
    fn mset_odd_args_error() {
        assert!(Command::from_value(array(&["MSET", "a", "1", "b"])).is_err());
    }

    #[test]
    fn setnx_parse() {
        assert_eq!(
            Command::from_value(array(&["SETNX", "k", "v"])).unwrap(),
            Command::SetNx("k".into(), "v".into())
        );
    }

    #[test]
    fn setex_parse() {
        assert_eq!(
            Command::from_value(array(&["SETEX", "k", "60", "v"])).unwrap(),
            Command::SetEx("k".into(), 60, "v".into())
        );
    }

    #[test]
    fn psetex_parse() {
        assert_eq!(
            Command::from_value(array(&["PSETEX", "k", "5000", "v"])).unwrap(),
            Command::PSetEx("k".into(), 5000, "v".into())
        );
    }

    #[test]
    fn incr_decr_parse() {
        assert_eq!(
            Command::from_value(array(&["INCR", "k"])).unwrap(),
            Command::Incr("k".into())
        );
        assert_eq!(
            Command::from_value(array(&["DECR", "k"])).unwrap(),
            Command::Decr("k".into())
        );
    }

    #[test]
    fn incrby_decrby_parse() {
        assert_eq!(
            Command::from_value(array(&["INCRBY", "k", "5"])).unwrap(),
            Command::IncrBy("k".into(), 5)
        );
        assert_eq!(
            Command::from_value(array(&["DECRBY", "k", "3"])).unwrap(),
            Command::DecrBy("k".into(), 3)
        );
    }

    // ── Expiry (parsing) ──────────────────────────────────────────────────────

    #[test]
    fn expire_pexpire_parse() {
        assert_eq!(
            Command::from_value(array(&["EXPIRE", "k", "60"])).unwrap(),
            Command::Expire("k".into(), 60)
        );
        assert_eq!(
            Command::from_value(array(&["PEXPIRE", "k", "5000"])).unwrap(),
            Command::PExpire("k".into(), 5000)
        );
    }

    #[test]
    fn expireat_pexpireat_parse() {
        assert_eq!(
            Command::from_value(array(&["EXPIREAT", "k", "9999999999"])).unwrap(),
            Command::ExpireAt("k".into(), 9999999999)
        );
        assert_eq!(
            Command::from_value(array(&["PEXPIREAT", "k", "9999999999000"])).unwrap(),
            Command::PExpireAt("k".into(), 9999999999000)
        );
    }

    #[test]
    fn ttl_pttl_parse() {
        assert_eq!(
            Command::from_value(array(&["TTL", "k"])).unwrap(),
            Command::Ttl("k".into())
        );
        assert_eq!(
            Command::from_value(array(&["PTTL", "k"])).unwrap(),
            Command::PTtl("k".into())
        );
    }

    #[test]
    fn persist_parse() {
        assert_eq!(
            Command::from_value(array(&["PERSIST", "k"])).unwrap(),
            Command::Persist("k".into())
        );
    }

    // ── Keys (parsing) ────────────────────────────────────────────────────────

    #[test]
    fn exists_parse() {
        assert_eq!(
            Command::from_value(array(&["EXISTS", "a", "b"])).unwrap(),
            Command::Exists(vec!["a".into(), "b".into()])
        );
    }

    #[test]
    fn keys_parse() {
        assert_eq!(
            Command::from_value(array(&["KEYS", "user:*"])).unwrap(),
            Command::Keys("user:*".into())
        );
    }

    #[test]
    fn scan_parse_with_count() {
        assert_eq!(
            Command::from_value(array(&["SCAN", "0", "MATCH", "*", "COUNT", "10"])).unwrap(),
            Command::Scan(0, Some("*".into()), Some(10))
        );
    }

    #[test]
    fn rename_parse() {
        assert_eq!(
            Command::from_value(array(&["RENAME", "src", "dst"])).unwrap(),
            Command::Rename("src".into(), "dst".into())
        );
    }

    #[test]
    fn type_parse() {
        assert_eq!(
            Command::from_value(array(&["TYPE", "k"])).unwrap(),
            Command::Type("k".into())
        );
    }

    #[test]
    fn dbsize_flushdb_parse() {
        assert_eq!(
            Command::from_value(array(&["DBSIZE"])).unwrap(),
            Command::DbSize
        );
        assert_eq!(
            Command::from_value(array(&["FLUSHDB"])).unwrap(),
            Command::FlushDb
        );
    }

    // ── Hash (parsing) ────────────────────────────────────────────────────────

    #[test]
    fn hkeys_hvals_hexists_parse() {
        assert_eq!(
            Command::from_value(array(&["HKEYS", "h"])).unwrap(),
            Command::HKeys("h".into())
        );
        assert_eq!(
            Command::from_value(array(&["HVALS", "h"])).unwrap(),
            Command::HVals("h".into())
        );
        assert_eq!(
            Command::from_value(array(&["HEXISTS", "h", "f"])).unwrap(),
            Command::HExists("h".into(), "f".into())
        );
    }

    #[test]
    fn hdel_multi_field_parse() {
        assert_eq!(
            Command::from_value(array(&["HDEL", "h", "f1", "f2"])).unwrap(),
            Command::HDel("h".into(), vec!["f1".into(), "f2".into()])
        );
    }

    #[test]
    fn hlen_parse() {
        assert_eq!(
            Command::from_value(array(&["HLEN", "h"])).unwrap(),
            Command::HLen("h".into())
        );
    }

    // ── List (parsing) ────────────────────────────────────────────────────────

    #[test]
    fn lpushx_rpushx_parse() {
        assert_eq!(
            Command::from_value(array(&["LPUSHX", "l", "v"])).unwrap(),
            Command::LPushX("l".into(), vec!["v".into()])
        );
        assert_eq!(
            Command::from_value(array(&["RPUSHX", "l", "v"])).unwrap(),
            Command::RPushX("l".into(), vec!["v".into()])
        );
    }

    #[test]
    fn lindex_lset_lrem_ltrim_parse() {
        assert_eq!(
            Command::from_value(array(&["LINDEX", "l", "0"])).unwrap(),
            Command::LIndex("l".into(), 0)
        );
        assert_eq!(
            Command::from_value(array(&["LSET", "l", "0", "v"])).unwrap(),
            Command::LSet("l".into(), 0, "v".into())
        );
        assert_eq!(
            Command::from_value(array(&["LREM", "l", "1", "v"])).unwrap(),
            Command::LRem("l".into(), 1, "v".into())
        );
        assert_eq!(
            Command::from_value(array(&["LTRIM", "l", "0", "9"])).unwrap(),
            Command::LTrim("l".into(), 0, 9)
        );
    }

    // ── Set (parsing) ─────────────────────────────────────────────────────────

    #[test]
    fn spop_parse() {
        assert_eq!(
            Command::from_value(array(&["SPOP", "s"])).unwrap(),
            Command::SPop("s".into(), None)
        );
        assert_eq!(
            Command::from_value(array(&["SPOP", "s", "3"])).unwrap(),
            Command::SPop("s".into(), Some(3))
        );
    }

    #[test]
    fn srandmember_parse() {
        assert_eq!(
            Command::from_value(array(&["SRANDMEMBER", "s"])).unwrap(),
            Command::SRandMember("s".into(), None)
        );
        assert_eq!(
            Command::from_value(array(&["SRANDMEMBER", "s", "-5"])).unwrap(),
            Command::SRandMember("s".into(), Some(-5))
        );
    }

    #[test]
    fn sinterstore_sdiffstore_parse() {
        assert_eq!(
            Command::from_value(array(&["SINTERSTORE", "dst", "s1", "s2"])).unwrap(),
            Command::SInterStore("dst".into(), vec!["s1".into(), "s2".into()])
        );
        assert_eq!(
            Command::from_value(array(&["SDIFFSTORE", "dst", "s1", "s2"])).unwrap(),
            Command::SDiffStore("dst".into(), vec!["s1".into(), "s2".into()])
        );
        assert_eq!(
            Command::from_value(array(&["SUNIONSTORE", "dst", "s1"])).unwrap(),
            Command::SUnionStore("dst".into(), vec!["s1".into()])
        );
    }

    // ── Sorted Set (parsing) ──────────────────────────────────────────────────

    #[test]
    fn zmscore_parse() {
        assert_eq!(
            Command::from_value(array(&["ZMSCORE", "z", "a", "b"])).unwrap(),
            Command::ZMScore("z".into(), vec!["a".into(), "b".into()])
        );
    }

    #[test]
    fn zrevrangebyscore_parse() {
        assert_eq!(
            Command::from_value(array(&["ZREVRANGEBYSCORE", "z", "+inf", "-inf"])).unwrap(),
            Command::ZRevRangeByScore("z".into(), "+inf".into(), "-inf".into(), false, None)
        );
    }

    #[test]
    fn zrevrangebyscore_with_limit_parse() {
        let cmd = Command::from_value(array(&[
            "ZREVRANGEBYSCORE",
            "z",
            "100",
            "0",
            "LIMIT",
            "0",
            "5",
        ]))
        .unwrap();
        assert_eq!(
            cmd,
            Command::ZRevRangeByScore("z".into(), "100".into(), "0".into(), false, Some((0, 5)))
        );
    }

    #[test]
    fn zrevrange_parse() {
        assert_eq!(
            Command::from_value(array(&["ZREVRANGE", "z", "0", "-1"])).unwrap(),
            Command::ZRevRange("z".into(), 0, -1, false)
        );
    }

    #[test]
    fn zrank_zrevrank_zcard_zcount_parse() {
        assert_eq!(
            Command::from_value(array(&["ZRANK", "z", "m"])).unwrap(),
            Command::ZRank("z".into(), "m".into())
        );
        assert_eq!(
            Command::from_value(array(&["ZREVRANK", "z", "m"])).unwrap(),
            Command::ZRevRank("z".into(), "m".into())
        );
        assert_eq!(
            Command::from_value(array(&["ZCARD", "z"])).unwrap(),
            Command::ZCard("z".into())
        );
        assert_eq!(
            Command::from_value(array(&["ZCOUNT", "z", "-inf", "+inf"])).unwrap(),
            Command::ZCount("z".into(), "-inf".into(), "+inf".into())
        );
    }

    // ── Rate limiting ───────────────────────────────────────────────────────

    #[test]
    fn rlset_parse() {
        assert_eq!(
            Command::from_value(array(&["RLSET", "api", "100", "60"])).unwrap(),
            Command::RlSet("api".into(), 100, 60)
        );
    }

    #[test]
    fn rlset_rejects_non_positive_config() {
        assert!(Command::from_value(array(&["RLSET", "api", "0", "60"])).is_err());
        assert!(Command::from_value(array(&["RLSET", "api", "10", "0"])).is_err());
        assert!(Command::from_value(array(&["RLSET", "api", "-1", "60"])).is_err());
    }

    #[test]
    fn rlcheck_bare_parse() {
        assert_eq!(
            Command::from_value(array(&["RLCHECK", "api"])).unwrap(),
            Command::RlCheck("api".into(), None)
        );
    }

    #[test]
    fn rlcheck_inline_config_parse() {
        assert_eq!(
            Command::from_value(array(&["RLCHECK", "ip:1.2.3.4", "5", "60"])).unwrap(),
            Command::RlCheck("ip:1.2.3.4".into(), Some((5, 60)))
        );
    }

    #[test]
    fn rlcheck_partial_config_errors() {
        assert!(Command::from_value(array(&["RLCHECK", "api", "5"])).is_err());
        assert!(Command::from_value(array(&["RLCHECK"])).is_err());
    }

    // ── Persistence ───────────────────────────────────────────────────────────

    #[test]
    fn save_bgsave_lastsave_parse() {
        assert_eq!(
            Command::from_value(array(&["SAVE"])).unwrap(),
            Command::Save
        );
        assert_eq!(
            Command::from_value(array(&["BGSAVE"])).unwrap(),
            Command::BgSave
        );
        assert_eq!(
            Command::from_value(array(&["LASTSAVE"])).unwrap(),
            Command::LastSave
        );
    }
}
