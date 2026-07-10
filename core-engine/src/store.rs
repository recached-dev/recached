use crate::cmd::{Command, SetCondition, SetExpiry, ZAddOptions};
use crate::resp::Value;
use dashmap::DashMap;
use indexmap::IndexSet;
use rand::Rng;
use rand::seq::IteratorRandom;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const WRONGTYPE: &str = "WRONGTYPE Operation against a key holding the wrong kind of value";

// ── time ──────────────────────────────────────────────────────────────────────

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── ZSet inner ────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct ZSetInner {
    pub scores: HashMap<String, f64>,
}

impl ZSetInner {
    fn new() -> Self {
        Self {
            scores: HashMap::new(),
        }
    }

    /// Members sorted by (score ASC, member ASC).
    fn rank_asc(&self) -> Vec<(&str, f64)> {
        let mut v: Vec<(&str, f64)> = self.scores.iter().map(|(m, &s)| (m.as_str(), s)).collect();
        v.sort_by(|(m1, s1), (m2, s2)| {
            s1.partial_cmp(s2)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(m1.cmp(m2))
        });
        v
    }
}

// ── score bounds ──────────────────────────────────────────────────────────────

enum ScoreBound {
    NegInf,
    PosInf,
    Inclusive(f64),
    Exclusive(f64),
}

impl ScoreBound {
    fn parse(s: &str) -> Result<Self, Value> {
        if s == "-inf" {
            Ok(Self::NegInf)
        } else if s == "+inf" || s == "inf" {
            Ok(Self::PosInf)
        } else if let Some(rest) = s.strip_prefix('(') {
            rest.parse::<f64>()
                .map(Self::Exclusive)
                .map_err(|_| Value::Error("ERR min or max is not a float".to_string()))
        } else {
            s.parse::<f64>()
                .map(Self::Inclusive)
                .map_err(|_| Value::Error("ERR min or max is not a float".to_string()))
        }
    }
}

fn in_score_range(score: f64, min: &ScoreBound, max: &ScoreBound) -> bool {
    let above = match min {
        ScoreBound::NegInf => true,
        ScoreBound::PosInf => false,
        ScoreBound::Inclusive(v) => score >= *v,
        ScoreBound::Exclusive(v) => score > *v,
    };
    let below = match max {
        ScoreBound::PosInf => true,
        ScoreBound::NegInf => false,
        ScoreBound::Inclusive(v) => score <= *v,
        ScoreBound::Exclusive(v) => score < *v,
    };
    above && below
}

// ── Entry value type ──────────────────────────────────────────────────────────

#[derive(Clone)]
enum EntryValue {
    Str(String),
    Hash(HashMap<String, String>),
    List(VecDeque<String>),
    // IndexSet rather than HashSet: SPOP / SRANDMEMBER need O(1) access to a
    // random member by index, which a hash table cannot provide.
    Set(IndexSet<String>),
    ZSet(ZSetInner),
    RateLimiter(RateLimiterInner),
    Json(serde_json::Value),
}

impl EntryValue {
    fn type_name(&self) -> &'static str {
        match self {
            EntryValue::Str(_) => "string",
            EntryValue::Hash(_) => "hash",
            EntryValue::List(_) => "list",
            EntryValue::Set(_) => "set",
            EntryValue::ZSet(_) => "zset",
            EntryValue::RateLimiter(_) => "ratelimit",
            EntryValue::Json(_) => "json",
        }
    }
}

// ── JSON paths & merge (JSET / JGET / JMERGE) ─────────────────────────────────

enum JsonPathSeg {
    Field(String),
    Index(usize),
}

const MAX_JSON_PATH_DEPTH: usize = 128;

/// Parse a deterministic JSON path: `$` (whole document), `$.user.name`,
/// `$.items[2].qty`. The leading `$` is optional. Wildcards, slices, and
/// filters are not supported — every path addresses exactly one location.
fn parse_json_path(path: &str) -> Result<Vec<JsonPathSeg>, String> {
    let mut rest = path.strip_prefix('$').unwrap_or(path);
    let mut segs = Vec::new();
    while !rest.is_empty() {
        if segs.len() >= MAX_JSON_PATH_DEPTH {
            return Err("ERR JSON path too deep".to_string());
        }
        if let Some(r) = rest.strip_prefix('.') {
            let end = r.find(['.', '[']).unwrap_or(r.len());
            let field = &r[..end];
            if field.is_empty() {
                return Err("ERR invalid JSON path: empty field".to_string());
            }
            segs.push(JsonPathSeg::Field(field.to_string()));
            rest = &r[end..];
        } else if let Some(r) = rest.strip_prefix('[') {
            let end = r
                .find(']')
                .ok_or_else(|| "ERR invalid JSON path: unterminated index".to_string())?;
            let idx: usize = r[..end]
                .parse()
                .map_err(|_| "ERR invalid JSON path: bad index".to_string())?;
            segs.push(JsonPathSeg::Index(idx));
            rest = &r[end + 1..];
        } else {
            // Bare leading field, e.g. `user.name` without `$.`.
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            segs.push(JsonPathSeg::Field(rest[..end].to_string()));
            rest = &rest[end..];
        }
    }
    Ok(segs)
}

/// Set `value` at the path. Intermediate objects are auto-created when a
/// field segment hits `null`; array indices must already exist.
fn json_set_at(
    cur: &mut serde_json::Value,
    segs: &[JsonPathSeg],
    value: serde_json::Value,
) -> Result<(), String> {
    let Some((seg, rest)) = segs.split_first() else {
        *cur = value;
        return Ok(());
    };
    match seg {
        JsonPathSeg::Field(f) => {
            if cur.is_null() {
                *cur = serde_json::Value::Object(serde_json::Map::new());
            }
            let obj = cur
                .as_object_mut()
                .ok_or_else(|| format!("ERR path segment '.{}' is not an object", f))?;
            let slot = obj.entry(f.clone()).or_insert(serde_json::Value::Null);
            json_set_at(slot, rest, value)
        }
        JsonPathSeg::Index(i) => {
            let arr = cur
                .as_array_mut()
                .ok_or_else(|| format!("ERR path segment '[{}]' is not an array", i))?;
            let len = arr.len();
            let slot = arr
                .get_mut(*i)
                .ok_or_else(|| format!("ERR index {} out of bounds (len {})", i, len))?;
            json_set_at(slot, rest, value)
        }
    }
}

fn json_get_at<'a>(
    cur: &'a serde_json::Value,
    segs: &[JsonPathSeg],
) -> Option<&'a serde_json::Value> {
    let mut c = cur;
    for seg in segs {
        c = match seg {
            JsonPathSeg::Field(f) => c.get(f.as_str())?,
            JsonPathSeg::Index(i) => c.get(*i)?,
        };
    }
    Some(c)
}

/// RFC 7386 JSON Merge Patch: objects merge recursively, `null` removes the
/// field, and any non-object patch replaces the target wholesale.
fn json_merge_patch(target: &mut serde_json::Value, patch: serde_json::Value) {
    match patch {
        serde_json::Value::Object(pobj) => {
            if !target.is_object() {
                *target = serde_json::Value::Object(serde_json::Map::new());
            }
            let tobj = target.as_object_mut().expect("just ensured object");
            for (k, v) in pobj {
                if v.is_null() {
                    tobj.remove(&k);
                } else {
                    let slot = tobj.entry(k).or_insert(serde_json::Value::Null);
                    json_merge_patch(slot, v);
                }
            }
        }
        other => *target = other,
    }
}

fn json_approx_size(v: &serde_json::Value) -> usize {
    use serde_json::Value as J;
    match v {
        J::Null | J::Bool(_) => 8,
        J::Number(_) => 16,
        J::String(s) => s.len() + 8,
        J::Array(a) => 8 + a.iter().map(json_approx_size).sum::<usize>(),
        J::Object(o) => {
            8 + o
                .iter()
                .map(|(k, val)| k.len() + json_approx_size(val))
                .sum::<usize>()
        }
    }
}

// ── Sliding-window rate limiter (RLSET / RLCHECK) ─────────────────────────────

#[derive(Clone)]
struct RateLimiterInner {
    limit: u64,
    window_ms: u64,
    /// Timestamps (ms) of recorded attempts, oldest first. Attempts arrive in
    /// monotonically non-decreasing time, so the deque stays sorted and window
    /// pruning is O(pruned) pops from the front — no sorted-set machinery
    /// needed. Length is bounded by `limit` (denied attempts are not recorded).
    events: VecDeque<u64>,
}

impl RateLimiterInner {
    fn new(limit: u64, window_ms: u64) -> Self {
        Self {
            limit,
            window_ms,
            events: VecDeque::new(),
        }
    }

    /// Record an attempt at `now`, returning `(allowed, remaining, retry_after_ms)`.
    /// Denied attempts are not recorded — a client hammering a full limiter
    /// does not push its own recovery further away.
    fn check(&mut self, now: u64) -> (i64, u64, u64) {
        let cutoff = now.saturating_sub(self.window_ms);
        while self.events.front().is_some_and(|&t| t <= cutoff) {
            self.events.pop_front();
        }
        if (self.events.len() as u64) < self.limit {
            self.events.push_back(now);
            let remaining = self.limit - self.events.len() as u64;
            (1, remaining, 0)
        } else {
            let retry_after = self
                .events
                .front()
                .map(|&t| (t + self.window_ms).saturating_sub(now))
                .unwrap_or(0);
            (0, 0, retry_after)
        }
    }
}

// ── Entry ─────────────────────────────────────────────────────────────────────

struct Entry {
    value: EntryValue,
    expires_at_ms: Option<u64>,
    /// Last time this entry was read or written, in ms. Drives LRU eviction.
    /// Atomic so reads can refresh recency while holding only a shared
    /// (DashMap read-lock) reference — no writer lock on the GET path.
    last_access_ms: AtomicU64,
}

impl Clone for Entry {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            expires_at_ms: self.expires_at_ms,
            last_access_ms: AtomicU64::new(self.last_access_ms.load(Ordering::Relaxed)),
        }
    }
}

impl Entry {
    fn new_str(value: String) -> Self {
        Self {
            value: EntryValue::Str(value),
            expires_at_ms: None,
            last_access_ms: AtomicU64::new(now_ms()),
        }
    }

    fn new_str_ex(value: String, expires_at_ms: u64) -> Self {
        Self {
            value: EntryValue::Str(value),
            expires_at_ms: Some(expires_at_ms),
            last_access_ms: AtomicU64::new(now_ms()),
        }
    }

    fn is_expired(&self, now: u64) -> bool {
        self.expires_at_ms.is_some_and(|exp| now >= exp)
    }

    /// Mark this entry as just-used so LRU eviction treats it as recent.
    fn touch(&self, now: u64) {
        self.last_access_ms.store(now, Ordering::Relaxed);
    }
}

// ── resolve list range helpers ────────────────────────────────────────────────

/// Convert a possibly-negative index into an absolute index in `[0, len)`.
fn resolve_idx(idx: i64, len: usize) -> Option<usize> {
    let resolved = if idx >= 0 {
        idx as usize
    } else {
        (len as i64 + idx) as usize
    };
    if resolved < len { Some(resolved) } else { None }
}

/// Clamp `start..=stop` (both possibly negative) to valid slice bounds.
/// Returns `(start_inclusive, end_inclusive)` with `start <= end`, or `None` for empty.
fn resolve_range(start: i64, stop: i64, len: usize) -> Option<(usize, usize)> {
    if len == 0 {
        return None;
    }
    let len_i = len as i64;
    let s = (if start < 0 { len_i + start } else { start }).max(0) as usize;
    let e = (if stop < 0 { len_i + stop } else { stop }).min(len_i - 1);
    if e < 0 || s >= len || s > e as usize {
        None
    } else {
        Some((s, e as usize))
    }
}

// ── zset range helpers ────────────────────────────────────────────────────────

fn zrange_index<'a>(sorted: &'a [(&'a str, f64)], start: i64, stop: i64) -> &'a [(&'a str, f64)] {
    let len = sorted.len();
    match resolve_range(start, stop, len) {
        None => &[],
        Some((s, e)) => &sorted[s..=e],
    }
}

fn apply_limit<T: Clone>(items: Vec<T>, limit: Option<(i64, i64)>) -> Vec<T> {
    match limit {
        None => items,
        Some((offset, count)) => {
            let start = offset.max(0) as usize;
            if start >= items.len() {
                return vec![];
            }
            let slice = &items[start..];
            if count < 0 {
                slice.to_vec()
            } else {
                slice[..count.min(slice.len() as i64) as usize].to_vec()
            }
        }
    }
}

fn encode_zrange(items: &[(&str, f64)], withscores: bool) -> Value {
    let mut out: Vec<Value> = Vec::with_capacity(if withscores {
        items.len() * 2
    } else {
        items.len()
    });
    for (m, s) in items {
        out.push(Value::BulkString(Some(m.as_bytes().to_vec())));
        if withscores {
            out.push(Value::BulkString(Some(format_score(*s).into_bytes())));
        }
    }
    Value::Array(Some(out))
}

pub fn format_score(s: f64) -> String {
    if s == f64::INFINITY {
        "inf".to_string()
    } else if s == f64::NEG_INFINITY {
        "-inf".to_string()
    } else if s.fract() == 0.0 && s.abs() < 1e15 {
        format!("{}", s as i64)
    } else {
        format!("{}", s)
    }
}

// ── macro: check entry type and prepare for mutation ─────────────────────────

/// Emits code that:
///   1. Checks if the key holds the wrong type → return WRONGTYPE error.
///   2. Retrieves the expired flag.
///
/// After the macro, `was_expired` is bound; the immutable borrow of `lock` is released.
macro_rules! type_guard {
    ($lock:expr, $key:expr, $variant:pat, $now:expr) => {{
        let (ok, expired) = match $lock.get($key) {
            None => (true, false),
            Some(e) if e.is_expired($now) => (true, true),
            Some(e) => (matches!(&e.value, $variant), false),
        };
        if !ok {
            return Value::Error(WRONGTYPE.to_string());
        }
        expired
    }};
}

// ── EvictionPolicy ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Default)]
pub enum EvictionPolicy {
    #[default]
    NoEviction,
    AllKeysLru,
    AllKeysRandom,
    VolatileLru,
    VolatileTtl,
}

// ── KeyValueStore ─────────────────────────────────────────────────────────────

// ── Snapshot types ────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub enum SnapshotValue {
    Str(String),
    Hash(HashMap<String, String>),
    List(Vec<String>),
    Set(Vec<String>),
    ZSet(Vec<(String, f64)>),
    // Appended after the original variants: rmp-serde encodes variants by
    // index, so new variants must go last to keep old snapshots readable.
    RateLimiter {
        limit: u64,
        window_ms: u64,
        events: Vec<u64>,
    },
    /// JSON document, stored serialized.
    Json(String),
}

#[derive(Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub key: String,
    pub value: SnapshotValue,
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone)]
pub struct KeyValueStore {
    data: Arc<DashMap<String, Entry>>,
    max_keys: Option<usize>,
    max_memory_bytes: Option<usize>,
    eviction_policy: EvictionPolicy,
    dirty: Arc<AtomicU64>,
}

impl Default for KeyValueStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyValueStore {
    pub fn new() -> Self {
        Self {
            data: Arc::new(DashMap::new()),
            max_keys: None,
            max_memory_bytes: None,
            eviction_policy: EvictionPolicy::NoEviction,
            dirty: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn with_max_keys(max: usize) -> Self {
        Self {
            data: Arc::new(DashMap::new()),
            max_keys: Some(max),
            max_memory_bytes: None,
            eviction_policy: EvictionPolicy::NoEviction,
            dirty: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn with_config(
        max_keys: Option<usize>,
        max_memory_bytes: Option<usize>,
        eviction_policy: EvictionPolicy,
    ) -> Self {
        Self {
            data: Arc::new(DashMap::new()),
            max_keys,
            max_memory_bytes,
            eviction_policy,
            dirty: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Number of write commands applied since the last `reset_dirty()`.
    pub fn dirty_count(&self) -> u64 {
        self.dirty.load(Ordering::Relaxed)
    }

    /// Reset the dirty counter to zero (call after a successful snapshot save).
    pub fn reset_dirty(&self) {
        self.dirty.store(0, Ordering::Relaxed);
    }

    /// Increment the dirty counter by one. Called by the server after every
    /// successful write command so the autosave loop can skip saves when
    /// nothing has changed.
    pub fn mark_dirty(&self) {
        self.dirty.fetch_add(1, Ordering::Relaxed);
    }

    /// Approximate heap usage in bytes — key+value sizes plus a fixed overhead per entry.
    pub fn approximate_memory_bytes(&self) -> usize {
        self.data
            .iter()
            .map(|r| entry_size(r.key(), r.value()))
            .sum()
    }

    /// Evict entries until memory usage is below `max_memory_bytes`, or the
    /// eviction policy cannot free any more. Returns true if under limit.
    ///
    /// The total is scanned once up front, then maintained incrementally by
    /// subtracting each evicted entry's measured size. The previous version
    /// re-scanned the whole keyspace after every single eviction (O(N) per
    /// eviction → O(N²) overall) — catastrophic under memory pressure. We
    /// periodically re-scan to correct for drift from concurrent writes.
    pub fn try_evict_for_memory(&self) -> bool {
        let limit = match self.max_memory_bytes {
            Some(l) => l,
            None => return true,
        };
        let now = now_ms();
        let mut current = self.approximate_memory_bytes();
        let mut since_resync = 0u32;
        while current > limit {
            match self.evict_one(now) {
                None => return false, // policy can't free anything more
                Some(freed) => {
                    current = current.saturating_sub(freed);
                    since_resync += 1;
                    if since_resync >= 64 {
                        current = self.approximate_memory_bytes();
                        since_resync = 0;
                    }
                }
            }
        }
        true
    }

    /// Returns the current value of a key for watch push notifications.
    /// Strings are returned as bulk strings. Complex types return nil — the
    /// watcher must use a type-specific command (HGETALL, LRANGE, etc.) to
    /// fetch the full value. Deleted or expired keys also return nil.
    pub fn get_current(&self, key: &str) -> Value {
        let now = now_ms();
        match self.data.get(key) {
            None => Value::BulkString(None),
            Some(e) if e.is_expired(now) => Value::BulkString(None),
            Some(e) => match &e.value {
                EntryValue::Str(s) => Value::BulkString(Some(s.clone().into_bytes())),
                EntryValue::Hash(_) => Value::SimpleString("hash".to_string()),
                EntryValue::List(_) => Value::SimpleString("list".to_string()),
                EntryValue::Set(_) => Value::SimpleString("set".to_string()),
                EntryValue::ZSet(_) => Value::SimpleString("zset".to_string()),
                EntryValue::RateLimiter(_) => Value::SimpleString("ratelimit".to_string()),
                EntryValue::Json(_) => Value::SimpleString("json".to_string()),
            },
        }
    }

    /// Current state of every live key matching the glob pattern, in
    /// `get_current` form (strings in full; collection types as type-name
    /// markers), capped at `limit` entries. Backs live-query (QSUB) initial
    /// state.
    pub fn matching_key_values(&self, pattern: &str, limit: usize) -> Vec<(String, Value)> {
        let now = now_ms();
        self.data
            .iter()
            .filter(|e| !e.is_expired(now) && glob_match(pattern, e.key()))
            .take(limit)
            .map(|e| {
                let value = match &e.value {
                    EntryValue::Str(s) => Value::BulkString(Some(s.clone().into_bytes())),
                    EntryValue::Hash(_) => Value::SimpleString("hash".to_string()),
                    EntryValue::List(_) => Value::SimpleString("list".to_string()),
                    EntryValue::Set(_) => Value::SimpleString("set".to_string()),
                    EntryValue::ZSet(_) => Value::SimpleString("zset".to_string()),
                    EntryValue::RateLimiter(_) => Value::SimpleString("ratelimit".to_string()),
                    EntryValue::Json(_) => Value::SimpleString("json".to_string()),
                };
                (e.key().clone(), value)
            })
            .collect()
    }

    pub fn sweep_expired(&self) {
        let now = now_ms();
        self.data.retain(|_, e| !e.is_expired(now));
    }

    /// Evict a single entry per the configured policy. Returns the number of
    /// bytes freed (`Some`), or `None` if nothing could be evicted.
    fn evict_one(&self, now: u64) -> Option<usize> {
        const SAMPLE: usize = 10;
        let mut rng = rand::rng();
        let chosen: Option<String> = match self.eviction_policy {
            EvictionPolicy::NoEviction => None,
            EvictionPolicy::AllKeysLru => {
                let sample: Vec<(String, u64)> = self
                    .data
                    .iter()
                    .map(|r| {
                        (
                            r.key().clone(),
                            r.value().last_access_ms.load(Ordering::Relaxed),
                        )
                    })
                    .choose_multiple(&mut rng, SAMPLE);
                sample.into_iter().min_by_key(|(_, w)| *w).map(|(k, _)| k)
            }
            EvictionPolicy::AllKeysRandom => {
                self.data.iter().map(|r| r.key().clone()).choose(&mut rng)
            }
            EvictionPolicy::VolatileLru => {
                let sample: Vec<(String, u64)> = self
                    .data
                    .iter()
                    .filter(|r| r.value().expires_at_ms.is_some() && !r.value().is_expired(now))
                    .map(|r| {
                        (
                            r.key().clone(),
                            r.value().last_access_ms.load(Ordering::Relaxed),
                        )
                    })
                    .choose_multiple(&mut rng, SAMPLE);
                sample.into_iter().min_by_key(|(_, w)| *w).map(|(k, _)| k)
            }
            EvictionPolicy::VolatileTtl => {
                let sample: Vec<(String, u64)> = self
                    .data
                    .iter()
                    .filter_map(|r| {
                        let exp = r.value().expires_at_ms?;
                        if r.value().is_expired(now) {
                            None
                        } else {
                            Some((r.key().clone(), exp))
                        }
                    })
                    .choose_multiple(&mut rng, SAMPLE);
                sample
                    .into_iter()
                    .min_by_key(|(_, exp)| *exp)
                    .map(|(k, _)| k)
            }
        };
        let key = chosen?;
        // Treat a lost race (key already gone) as a successful eviction that
        // freed nothing, so callers don't spin.
        Some(
            self.data
                .remove(&key)
                .map_or(0, |(k, e)| entry_size(&k, &e)),
        )
    }

    pub fn snapshot(&self) -> Vec<SnapshotEntry> {
        let now = now_ms();
        self.data
            .iter()
            .filter(|e| !e.is_expired(now))
            .map(|e| {
                let value = match &e.value {
                    EntryValue::Str(s) => SnapshotValue::Str(s.clone()),
                    EntryValue::Hash(m) => SnapshotValue::Hash(m.clone()),
                    EntryValue::List(l) => SnapshotValue::List(l.iter().cloned().collect()),
                    EntryValue::Set(s) => SnapshotValue::Set(s.iter().cloned().collect()),
                    EntryValue::ZSet(z) => {
                        SnapshotValue::ZSet(z.scores.iter().map(|(k, &v)| (k.clone(), v)).collect())
                    }
                    EntryValue::RateLimiter(rl) => SnapshotValue::RateLimiter {
                        limit: rl.limit,
                        window_ms: rl.window_ms,
                        events: rl.events.iter().copied().collect(),
                    },
                    EntryValue::Json(doc) => {
                        SnapshotValue::Json(serde_json::to_string(doc).unwrap_or_default())
                    }
                };
                SnapshotEntry {
                    key: e.key().clone(),
                    value,
                    expires_at_ms: e.expires_at_ms,
                }
            })
            .collect()
    }

    pub fn restore(&self, entries: Vec<SnapshotEntry>) {
        let now = now_ms();
        for e in entries {
            if let Some(exp) = e.expires_at_ms
                && now >= exp
            {
                continue;
            }
            let value = match e.value {
                SnapshotValue::Str(s) => EntryValue::Str(s),
                SnapshotValue::Hash(m) => EntryValue::Hash(m),
                SnapshotValue::List(l) => EntryValue::List(l.into_iter().collect()),
                SnapshotValue::Set(s) => EntryValue::Set(s.into_iter().collect()),
                SnapshotValue::ZSet(pairs) => EntryValue::ZSet(ZSetInner {
                    scores: pairs.into_iter().collect(),
                }),
                SnapshotValue::RateLimiter {
                    limit,
                    window_ms,
                    events,
                } => EntryValue::RateLimiter(RateLimiterInner {
                    limit,
                    window_ms,
                    events: events.into(),
                }),
                SnapshotValue::Json(s) => {
                    EntryValue::Json(serde_json::from_str(&s).unwrap_or(serde_json::Value::Null))
                }
            };
            self.data.insert(
                e.key,
                Entry {
                    value,
                    expires_at_ms: e.expires_at_ms,
                    last_access_ms: AtomicU64::new(now),
                },
            );
        }
    }

    pub fn execute(&self, cmd: Command) -> Value {
        match cmd {
            // ── Core ─────────────────────────────────────────────────────────
            Command::Ping(msg) => match msg {
                Some(m) => Value::BulkString(Some(m.into_bytes())),
                None => Value::SimpleString("PONG".to_string()),
            },
            Command::Auth(_) => Value::Error(
                "ERR AUTH is handled by the connection layer, not the store".to_string(),
            ),

            // ── Strings ───────────────────────────────────────────────────────
            Command::Set(key, val, opts) => {
                let now = now_ms();

                let (key_exists, existing_str, existing_ttl, wrongtype) = {
                    match self.data.get(&key) {
                        None => (false, None, None, false),
                        Some(e) if e.is_expired(now) => (false, None, None, false),
                        Some(e) => match &e.value {
                            EntryValue::Str(s) => (true, Some(s.clone()), e.expires_at_ms, false),
                            _ => (true, None, e.expires_at_ms, opts.get),
                        },
                    }
                };

                if wrongtype {
                    return Value::Error(WRONGTYPE.to_string());
                }

                let condition_met = match &opts.condition {
                    Some(SetCondition::Nx) => !key_exists,
                    Some(SetCondition::Xx) => key_exists,
                    None => true,
                };

                if !condition_met {
                    return if opts.get {
                        existing_str
                            .map(|v| Value::BulkString(Some(v.into_bytes())))
                            .unwrap_or(Value::BulkString(None))
                    } else {
                        Value::BulkString(None)
                    };
                }

                if let Some(max) = self.max_keys
                    && self.data.len() >= max
                    && !self.data.contains_key(&key)
                    && self.evict_one(now).is_none()
                {
                    return Value::Error("ERR max keys limit reached".to_string());
                }

                let expires_at_ms = match &opts.expiry {
                    None => None,
                    Some(SetExpiry::Ex(s)) => {
                        if *s > u64::MAX / 1000 {
                            return Value::Error("ERR TTL overflow".to_string());
                        }
                        Some(now.saturating_add(s * 1000))
                    }
                    Some(SetExpiry::Px(ms)) => Some(now.saturating_add(*ms)),
                    Some(SetExpiry::Exat(ts)) => Some(ts.saturating_mul(1000)),
                    Some(SetExpiry::Pxat(ts_ms)) => Some(*ts_ms),
                    Some(SetExpiry::KeepTtl) => existing_ttl,
                };

                self.data.insert(
                    key,
                    Entry {
                        value: EntryValue::Str(val),
                        expires_at_ms,
                        last_access_ms: AtomicU64::new(now),
                    },
                );

                if opts.get {
                    existing_str
                        .map(|v| Value::BulkString(Some(v.into_bytes())))
                        .unwrap_or(Value::BulkString(None))
                } else {
                    Value::SimpleString("OK".to_string())
                }
            }

            Command::Get(key) => {
                let now = now_ms();
                match self.data.get(&key) {
                    Some(e) if !e.is_expired(now) => match &e.value {
                        EntryValue::Str(s) => {
                            e.touch(now);
                            Value::BulkString(Some(s.clone().into_bytes()))
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                    _ => Value::BulkString(None),
                }
            }

            Command::Del(keys) | Command::Unlink(keys) => {
                let now = now_ms();
                let count = keys
                    .into_iter()
                    .filter(|k| self.data.remove_if(k, |_, e| !e.is_expired(now)).is_some())
                    .count();
                Value::Integer(count as i64)
            }

            Command::Append(key, suffix) => {
                let now = now_ms();
                let was_expired = type_guard!(self.data, &key, EntryValue::Str(_), now);
                let mut entry = self
                    .data
                    .entry(key)
                    .or_insert_with(|| Entry::new_str(String::new()));
                if was_expired {
                    entry.value = EntryValue::Str(String::new());
                    entry.expires_at_ms = None;
                }
                match &mut entry.value {
                    EntryValue::Str(s) => {
                        s.push_str(&suffix);
                        Value::Integer(s.len() as i64)
                    }
                    _ => unreachable!(),
                }
            }

            Command::Strlen(key) => {
                let now = now_ms();
                match self.data.get(&key) {
                    Some(e) if !e.is_expired(now) => match &e.value {
                        EntryValue::Str(s) => Value::Integer(s.len() as i64),
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                    _ => Value::Integer(0),
                }
            }

            Command::GetSet(key, new_val) => {
                let now = now_ms();
                let old = match self.data.get(&key) {
                    Some(e) if !e.is_expired(now) => match &e.value {
                        EntryValue::Str(s) => Value::BulkString(Some(s.clone().into_bytes())),
                        _ => return Value::Error(WRONGTYPE.to_string()),
                    },
                    _ => Value::BulkString(None),
                };
                self.data.insert(key, Entry::new_str(new_val));
                old
            }

            Command::MGet(keys) => {
                let now = now_ms();
                let results = keys
                    .iter()
                    .map(|k| match self.data.get(k) {
                        Some(e) if !e.is_expired(now) => match &e.value {
                            EntryValue::Str(s) => {
                                e.touch(now);
                                Value::BulkString(Some(s.clone().into_bytes()))
                            }
                            _ => Value::BulkString(None),
                        },
                        _ => Value::BulkString(None),
                    })
                    .collect();
                Value::Array(Some(results))
            }

            Command::MSet(pairs) => {
                let now = now_ms();
                if let Some(max) = self.max_keys {
                    let new_count = pairs
                        .iter()
                        .filter(|(k, _)| !self.data.contains_key(k))
                        .count();
                    let available = max.saturating_sub(self.data.len());
                    if new_count > available {
                        let needed = new_count - available;
                        for _ in 0..needed {
                            if self.evict_one(now).is_none() {
                                return Value::Error("ERR max keys limit reached".to_string());
                            }
                        }
                    }
                }
                for (k, v) in pairs {
                    self.data.insert(k, Entry::new_str(v));
                }
                Value::SimpleString("OK".to_string())
            }

            Command::SetNx(key, val) => {
                let now = now_ms();
                let exists = self.data.get(&key).is_some_and(|e| !e.is_expired(now));
                if exists {
                    return Value::Integer(0);
                }
                if let Some(max) = self.max_keys
                    && self.data.len() >= max
                    && !self.data.contains_key(&key)
                    && self.evict_one(now).is_none()
                {
                    return Value::Error("ERR max keys limit reached".to_string());
                }
                self.data.insert(key, Entry::new_str(val));
                Value::Integer(1)
            }

            Command::SetEx(key, secs, val) => {
                let now = now_ms();
                let exp = now.saturating_add(secs.saturating_mul(1000));
                if let Some(max) = self.max_keys
                    && self.data.len() >= max
                    && !self.data.contains_key(&key)
                    && self.evict_one(now).is_none()
                {
                    return Value::Error("ERR max keys limit reached".to_string());
                }
                self.data.insert(key, Entry::new_str_ex(val, exp));
                Value::SimpleString("OK".to_string())
            }

            Command::PSetEx(key, ms, val) => {
                let now = now_ms();
                let exp = now.saturating_add(ms);
                if let Some(max) = self.max_keys
                    && self.data.len() >= max
                    && !self.data.contains_key(&key)
                    && self.evict_one(now).is_none()
                {
                    return Value::Error("ERR max keys limit reached".to_string());
                }
                self.data.insert(key, Entry::new_str_ex(val, exp));
                Value::SimpleString("OK".to_string())
            }

            Command::Incr(key) => incr_by(&self.data, key, 1),
            Command::Decr(key) => incr_by(&self.data, key, -1),
            Command::IncrBy(key, delta) => incr_by(&self.data, key, delta),
            Command::DecrBy(key, delta) => incr_by(&self.data, key, -delta),

            // ── Expiry ────────────────────────────────────────────────────────
            Command::Expire(key, secs) => set_expiry(
                &self.data,
                key,
                now_ms().saturating_add(secs.saturating_mul(1000)),
            ),
            Command::PExpire(key, ms) => set_expiry(&self.data, key, now_ms().saturating_add(ms)),
            Command::ExpireAt(key, ts) => set_expiry(&self.data, key, ts.saturating_mul(1000)),
            Command::PExpireAt(key, ts) => set_expiry(&self.data, key, ts),

            Command::Ttl(key) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Integer(-2),
                    Some(e) if e.is_expired(now) => Value::Integer(-2),
                    Some(e) => match e.expires_at_ms {
                        None => Value::Integer(-1),
                        Some(exp) => Value::Integer((exp.saturating_sub(now) / 1000) as i64),
                    },
                }
            }

            Command::PTtl(key) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Integer(-2),
                    Some(e) if e.is_expired(now) => Value::Integer(-2),
                    Some(e) => match e.expires_at_ms {
                        None => Value::Integer(-1),
                        Some(exp) => Value::Integer(exp.saturating_sub(now) as i64),
                    },
                }
            }

            Command::Persist(key) => {
                let now = now_ms();
                match self.data.get_mut(&key) {
                    Some(mut e) if !e.is_expired(now) && e.expires_at_ms.is_some() => {
                        e.expires_at_ms = None;
                        Value::Integer(1)
                    }
                    Some(e) if !e.is_expired(now) => Value::Integer(0),
                    _ => Value::Integer(0),
                }
            }

            // ── Keys ──────────────────────────────────────────────────────────
            Command::Exists(keys) => {
                let now = now_ms();
                let count = keys
                    .iter()
                    .filter(|k| self.data.get(*k).is_some_and(|e| !e.is_expired(now)))
                    .count();
                Value::Integer(count as i64)
            }

            Command::Keys(pattern) => {
                let now = now_ms();
                let mut keys: Vec<Value> = self
                    .data
                    .iter()
                    .filter(|r| !r.value().is_expired(now) && glob_match(&pattern, r.key()))
                    .map(|r| Value::BulkString(Some(r.key().as_bytes().to_vec())))
                    .collect();
                keys.sort_unstable_by(|a, b| {
                    let ka = if let Value::BulkString(Some(d)) = a {
                        d.as_slice()
                    } else {
                        &[]
                    };
                    let kb = if let Value::BulkString(Some(d)) = b {
                        d.as_slice()
                    } else {
                        &[]
                    };
                    ka.cmp(kb)
                });
                Value::Array(Some(keys))
            }

            Command::Scan(cursor, pattern, count) => {
                let now = now_ms();
                let pat = pattern.as_deref().unwrap_or("*");
                let batch = count.unwrap_or(10).max(1);
                // Sort for a stable order so the numeric cursor is a meaningful
                // offset across calls and COUNT actually paginates. This is
                // O(N log N) per call (like KEYS), but each reply is bounded to
                // `batch` keys instead of dumping the whole keyspace at once.
                // As with Redis, concurrent inserts/deletes between calls may
                // cause a key to be skipped or returned twice.
                let mut all: Vec<String> = self
                    .data
                    .iter()
                    .filter(|r| !r.value().is_expired(now) && glob_match(pat, r.key()))
                    .map(|r| r.key().clone())
                    .collect();
                all.sort_unstable();
                let start = cursor as usize;
                let end = start.saturating_add(batch).min(all.len());
                let page: &[String] = if start < all.len() {
                    &all[start..end]
                } else {
                    &[]
                };
                let next_cursor = if end >= all.len() { 0 } else { end as u64 };
                let out: Vec<Value> = page
                    .iter()
                    .map(|k| Value::BulkString(Some(k.as_bytes().to_vec())))
                    .collect();
                Value::Array(Some(vec![
                    Value::BulkString(Some(next_cursor.to_string().into_bytes())),
                    Value::Array(Some(out)),
                ]))
            }

            Command::DbSize => {
                let now = now_ms();
                Value::Integer(
                    self.data
                        .iter()
                        .filter(|r| !r.value().is_expired(now))
                        .count() as i64,
                )
            }

            Command::FlushDb => {
                self.data.clear();
                Value::SimpleString("OK".to_string())
            }

            Command::Rename(src, dst) => {
                let now = now_ms();
                match self.data.remove(&src) {
                    None => Value::Error("ERR no such key".to_string()),
                    Some((_, e)) if e.is_expired(now) => {
                        Value::Error("ERR no such key".to_string())
                    }
                    Some((_, entry)) => {
                        self.data.insert(dst, entry);
                        Value::SimpleString("OK".to_string())
                    }
                }
            }

            Command::Type(key) => {
                let now = now_ms();
                match self.data.get(&key) {
                    Some(e) if !e.is_expired(now) => {
                        Value::SimpleString(e.value.type_name().to_string())
                    }
                    _ => Value::SimpleString("none".to_string()),
                }
            }

            // ── Hash ──────────────────────────────────────────────────────────
            Command::HSet(key, pairs) => {
                let now = now_ms();
                let was_expired = type_guard!(self.data, &key, EntryValue::Hash(_), now);
                let mut entry = self.data.entry(key).or_insert_with(|| Entry {
                    value: EntryValue::Hash(HashMap::new()),
                    expires_at_ms: None,
                    last_access_ms: AtomicU64::new(now_ms()),
                });
                if was_expired {
                    entry.value = EntryValue::Hash(HashMap::new());
                    entry.expires_at_ms = None;
                }
                let h = match &mut entry.value {
                    EntryValue::Hash(h) => h,
                    _ => unreachable!(),
                };
                let new_count = pairs
                    .iter()
                    .filter(|(f, _)| !h.contains_key(f.as_str()))
                    .count();
                for (field, val) in pairs {
                    h.insert(field, val);
                }
                Value::Integer(new_count as i64)
            }

            Command::HGet(key, field) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::BulkString(None),
                    Some(e) if e.is_expired(now) => Value::BulkString(None),
                    Some(e) => match &e.value {
                        EntryValue::Hash(h) => {
                            e.touch(now);
                            h.get(&field)
                                .map(|v| Value::BulkString(Some(v.clone().into_bytes())))
                                .unwrap_or(Value::BulkString(None))
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::HGetAll(key) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Array(Some(vec![])),
                    Some(e) if e.is_expired(now) => Value::Array(Some(vec![])),
                    Some(e) => match &e.value {
                        EntryValue::Hash(h) => {
                            e.touch(now);
                            let mut pairs: Vec<(&str, &str)> =
                                h.iter().map(|(f, v)| (f.as_str(), v.as_str())).collect();
                            pairs.sort_unstable_by_key(|(f, _)| *f);
                            let out = pairs
                                .into_iter()
                                .flat_map(|(f, v)| {
                                    [
                                        Value::BulkString(Some(f.as_bytes().to_vec())),
                                        Value::BulkString(Some(v.as_bytes().to_vec())),
                                    ]
                                })
                                .collect();
                            Value::Array(Some(out))
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::HDel(key, fields) => {
                let now = now_ms();
                match self.data.get_mut(&key) {
                    None => Value::Integer(0),
                    Some(e) if e.is_expired(now) => Value::Integer(0),
                    Some(mut e) => match &mut e.value {
                        EntryValue::Hash(h) => {
                            let count =
                                fields.into_iter().filter(|f| h.remove(f).is_some()).count();
                            Value::Integer(count as i64)
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::HKeys(key) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Array(Some(vec![])),
                    Some(e) if e.is_expired(now) => Value::Array(Some(vec![])),
                    Some(e) => match &e.value {
                        EntryValue::Hash(h) => {
                            let mut keys: Vec<&str> = h.keys().map(|s| s.as_str()).collect();
                            keys.sort_unstable();
                            Value::Array(Some(
                                keys.into_iter()
                                    .map(|k| Value::BulkString(Some(k.as_bytes().to_vec())))
                                    .collect(),
                            ))
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::HVals(key) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Array(Some(vec![])),
                    Some(e) if e.is_expired(now) => Value::Array(Some(vec![])),
                    Some(e) => match &e.value {
                        EntryValue::Hash(h) => {
                            let mut pairs: Vec<(&str, &str)> =
                                h.iter().map(|(f, v)| (f.as_str(), v.as_str())).collect();
                            pairs.sort_unstable_by_key(|(f, _)| *f);
                            Value::Array(Some(
                                pairs
                                    .into_iter()
                                    .map(|(_, v)| Value::BulkString(Some(v.as_bytes().to_vec())))
                                    .collect(),
                            ))
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::HLen(key) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Integer(0),
                    Some(e) if e.is_expired(now) => Value::Integer(0),
                    Some(e) => match &e.value {
                        EntryValue::Hash(h) => Value::Integer(h.len() as i64),
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::HIncrBy(key, field, delta) => hash_incr_int(&self.data, key, field, delta),

            Command::HIncrByFloat(key, field, delta) => {
                hash_incr_float(&self.data, key, field, delta)
            }

            Command::HExists(key, field) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Integer(0),
                    Some(e) if e.is_expired(now) => Value::Integer(0),
                    Some(e) => match &e.value {
                        EntryValue::Hash(h) => {
                            Value::Integer(if h.contains_key(&field) { 1 } else { 0 })
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::HSetNx(key, field, val) => {
                let now = now_ms();
                let was_expired = type_guard!(self.data, &key, EntryValue::Hash(_), now);
                let mut entry = self.data.entry(key).or_insert_with(|| Entry {
                    value: EntryValue::Hash(HashMap::new()),
                    expires_at_ms: None,
                    last_access_ms: AtomicU64::new(now_ms()),
                });
                if was_expired {
                    entry.value = EntryValue::Hash(HashMap::new());
                    entry.expires_at_ms = None;
                }
                let h = match &mut entry.value {
                    EntryValue::Hash(h) => h,
                    _ => unreachable!(),
                };
                if let std::collections::hash_map::Entry::Vacant(e) = h.entry(field) {
                    e.insert(val);
                    Value::Integer(1)
                } else {
                    Value::Integer(0)
                }
            }

            Command::HMGet(key, fields) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Array(Some(
                        fields.iter().map(|_| Value::BulkString(None)).collect(),
                    )),
                    Some(e) if e.is_expired(now) => Value::Array(Some(
                        fields.iter().map(|_| Value::BulkString(None)).collect(),
                    )),
                    Some(e) => match &e.value {
                        EntryValue::Hash(h) => Value::Array(Some(
                            fields
                                .iter()
                                .map(|f| {
                                    h.get(f)
                                        .map(|v| Value::BulkString(Some(v.clone().into_bytes())))
                                        .unwrap_or(Value::BulkString(None))
                                })
                                .collect(),
                        )),
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            // ── List ──────────────────────────────────────────────────────────
            Command::LPush(key, vals) => {
                let now = now_ms();
                let was_expired = type_guard!(self.data, &key, EntryValue::List(_), now);
                let mut entry = self.data.entry(key).or_insert_with(|| Entry {
                    value: EntryValue::List(VecDeque::new()),
                    expires_at_ms: None,
                    last_access_ms: AtomicU64::new(now_ms()),
                });
                if was_expired {
                    entry.value = EntryValue::List(VecDeque::new());
                    entry.expires_at_ms = None;
                }
                let list = match &mut entry.value {
                    EntryValue::List(l) => l,
                    _ => unreachable!(),
                };
                for v in vals {
                    list.push_front(v);
                }
                Value::Integer(list.len() as i64)
            }

            Command::RPush(key, vals) => {
                let now = now_ms();
                let was_expired = type_guard!(self.data, &key, EntryValue::List(_), now);
                let mut entry = self.data.entry(key).or_insert_with(|| Entry {
                    value: EntryValue::List(VecDeque::new()),
                    expires_at_ms: None,
                    last_access_ms: AtomicU64::new(now_ms()),
                });
                if was_expired {
                    entry.value = EntryValue::List(VecDeque::new());
                    entry.expires_at_ms = None;
                }
                let list = match &mut entry.value {
                    EntryValue::List(l) => l,
                    _ => unreachable!(),
                };
                for v in vals {
                    list.push_back(v);
                }
                Value::Integer(list.len() as i64)
            }

            Command::LPushX(key, vals) => {
                let now = now_ms();
                match self.data.get_mut(&key) {
                    None => Value::Integer(0),
                    Some(e) if e.is_expired(now) => Value::Integer(0),
                    Some(mut e) => match &mut e.value {
                        EntryValue::List(list) => {
                            for v in vals {
                                list.push_front(v);
                            }
                            Value::Integer(list.len() as i64)
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::RPushX(key, vals) => {
                let now = now_ms();
                match self.data.get_mut(&key) {
                    None => Value::Integer(0),
                    Some(e) if e.is_expired(now) => Value::Integer(0),
                    Some(mut e) => match &mut e.value {
                        EntryValue::List(list) => {
                            for v in vals {
                                list.push_back(v);
                            }
                            Value::Integer(list.len() as i64)
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::LPop(key, count) => {
                let now = now_ms();
                match self.data.get_mut(&key) {
                    None => no_list_response(count),
                    Some(e) if e.is_expired(now) => no_list_response(count),
                    Some(mut e) => match &mut e.value {
                        EntryValue::List(list) => {
                            if let Some(n) = count {
                                let items: Vec<Value> = (0..n)
                                    .filter_map(|_| {
                                        list.pop_front()
                                            .map(|v| Value::BulkString(Some(v.into_bytes())))
                                    })
                                    .collect();
                                Value::Array(Some(items))
                            } else {
                                list.pop_front()
                                    .map(|v| Value::BulkString(Some(v.into_bytes())))
                                    .unwrap_or(Value::BulkString(None))
                            }
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::RPop(key, count) => {
                let now = now_ms();
                match self.data.get_mut(&key) {
                    None => no_list_response(count),
                    Some(e) if e.is_expired(now) => no_list_response(count),
                    Some(mut e) => match &mut e.value {
                        EntryValue::List(list) => {
                            if let Some(n) = count {
                                let items: Vec<Value> = (0..n)
                                    .filter_map(|_| {
                                        list.pop_back()
                                            .map(|v| Value::BulkString(Some(v.into_bytes())))
                                    })
                                    .collect();
                                Value::Array(Some(items))
                            } else {
                                list.pop_back()
                                    .map(|v| Value::BulkString(Some(v.into_bytes())))
                                    .unwrap_or(Value::BulkString(None))
                            }
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::LRange(key, start, stop) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Array(Some(vec![])),
                    Some(e) if e.is_expired(now) => Value::Array(Some(vec![])),
                    Some(e) => match &e.value {
                        EntryValue::List(list) => {
                            e.touch(now);
                            let slice: Vec<&String> = list.iter().collect();
                            match resolve_range(start, stop, slice.len()) {
                                None => Value::Array(Some(vec![])),
                                Some((s, e)) => Value::Array(Some(
                                    slice[s..=e]
                                        .iter()
                                        .map(|v| Value::BulkString(Some(v.as_bytes().to_vec())))
                                        .collect(),
                                )),
                            }
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::LLen(key) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Integer(0),
                    Some(e) if e.is_expired(now) => Value::Integer(0),
                    Some(e) => match &e.value {
                        EntryValue::List(l) => Value::Integer(l.len() as i64),
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::LIndex(key, idx) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::BulkString(None),
                    Some(e) if e.is_expired(now) => Value::BulkString(None),
                    Some(e) => match &e.value {
                        EntryValue::List(list) => {
                            let slice: Vec<&String> = list.iter().collect();
                            resolve_idx(idx, slice.len())
                                .map(|i| Value::BulkString(Some(slice[i].as_bytes().to_vec())))
                                .unwrap_or(Value::BulkString(None))
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::LSet(key, idx, val) => {
                let now = now_ms();
                match self.data.get_mut(&key) {
                    None => Value::Error("ERR no such key".to_string()),
                    Some(e) if e.is_expired(now) => Value::Error("ERR no such key".to_string()),
                    Some(mut e) => match &mut e.value {
                        EntryValue::List(list) => {
                            let len = list.len();
                            match resolve_idx(idx, len) {
                                None => Value::Error("ERR index out of range".to_string()),
                                Some(i) => {
                                    list[i] = val;
                                    Value::SimpleString("OK".to_string())
                                }
                            }
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::LRem(key, count, element) => {
                let now = now_ms();
                match self.data.get_mut(&key) {
                    None => Value::Integer(0),
                    Some(e) if e.is_expired(now) => Value::Integer(0),
                    Some(mut e) => match &mut e.value {
                        EntryValue::List(list) => {
                            let mut removed = 0i64;
                            let abs = count.unsigned_abs() as usize;
                            if count >= 0 {
                                let mut i = 0;
                                while i < list.len() && (count == 0 || removed < abs as i64) {
                                    if list[i] == element {
                                        list.remove(i);
                                        removed += 1;
                                    } else {
                                        i += 1;
                                    }
                                }
                            } else {
                                let mut i = list.len();
                                while i > 0 && removed < abs as i64 {
                                    i -= 1;
                                    if list[i] == element {
                                        list.remove(i);
                                        removed += 1;
                                    }
                                }
                            }
                            Value::Integer(removed)
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::LTrim(key, start, stop) => {
                let now = now_ms();
                match self.data.get_mut(&key) {
                    None => Value::SimpleString("OK".to_string()),
                    Some(e) if e.is_expired(now) => Value::SimpleString("OK".to_string()),
                    Some(mut e) => match &mut e.value {
                        EntryValue::List(list) => {
                            let len = list.len();
                            match resolve_range(start, stop, len) {
                                None => list.clear(),
                                Some((s, e)) => {
                                    let trimmed: VecDeque<String> = list.drain(s..=e).collect();
                                    *list = trimmed;
                                }
                            }
                            Value::SimpleString("OK".to_string())
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            // ── Set ───────────────────────────────────────────────────────────
            Command::SAdd(key, members) => {
                let now = now_ms();
                let was_expired = type_guard!(self.data, &key, EntryValue::Set(_), now);
                let mut entry = self.data.entry(key).or_insert_with(|| Entry {
                    value: EntryValue::Set(IndexSet::new()),
                    expires_at_ms: None,
                    last_access_ms: AtomicU64::new(now_ms()),
                });
                if was_expired {
                    entry.value = EntryValue::Set(IndexSet::new());
                    entry.expires_at_ms = None;
                }
                let set = match &mut entry.value {
                    EntryValue::Set(s) => s,
                    _ => unreachable!(),
                };
                let added = members
                    .into_iter()
                    .filter(|m| set.insert(m.clone()))
                    .count();
                Value::Integer(added as i64)
            }

            Command::SMembers(key) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Array(Some(vec![])),
                    Some(e) if e.is_expired(now) => Value::Array(Some(vec![])),
                    Some(e) => match &e.value {
                        EntryValue::Set(s) => {
                            e.touch(now);
                            let mut members: Vec<&str> = s.iter().map(|m| m.as_str()).collect();
                            members.sort_unstable();
                            Value::Array(Some(
                                members
                                    .into_iter()
                                    .map(|m| Value::BulkString(Some(m.as_bytes().to_vec())))
                                    .collect(),
                            ))
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::SRem(key, members) => {
                let now = now_ms();
                match self.data.get_mut(&key) {
                    None => Value::Integer(0),
                    Some(e) if e.is_expired(now) => Value::Integer(0),
                    Some(mut e) => match &mut e.value {
                        EntryValue::Set(s) => {
                            let removed = members.into_iter().filter(|m| s.swap_remove(m)).count();
                            Value::Integer(removed as i64)
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::SCard(key) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Integer(0),
                    Some(e) if e.is_expired(now) => Value::Integer(0),
                    Some(e) => match &e.value {
                        EntryValue::Set(s) => Value::Integer(s.len() as i64),
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::SIsMember(key, member) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Integer(0),
                    Some(e) if e.is_expired(now) => Value::Integer(0),
                    Some(e) => match &e.value {
                        EntryValue::Set(s) => {
                            e.touch(now);
                            Value::Integer(if s.contains(&member) { 1 } else { 0 })
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::SMIsMember(key, members) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Array(Some(members.iter().map(|_| Value::Integer(0)).collect())),
                    Some(e) if e.is_expired(now) => {
                        Value::Array(Some(members.iter().map(|_| Value::Integer(0)).collect()))
                    }
                    Some(e) => match &e.value {
                        EntryValue::Set(s) => Value::Array(Some(
                            members
                                .iter()
                                .map(|m| Value::Integer(if s.contains(m) { 1 } else { 0 }))
                                .collect(),
                        )),
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::SInter(keys) => {
                let now = now_ms();
                match set_inter(&self.data, &keys, now) {
                    Err(e) => e,
                    Ok(result) => set_to_value(result),
                }
            }

            Command::SInterStore(dst, keys) => {
                let now = now_ms();
                let result = {
                    match set_inter(&self.data, &keys, now) {
                        Err(e) => return e,
                        Ok(r) => r,
                    }
                };
                let len = result.len();
                self.data.insert(
                    dst,
                    Entry {
                        value: EntryValue::Set(result),
                        expires_at_ms: None,
                        last_access_ms: AtomicU64::new(now_ms()),
                    },
                );
                Value::Integer(len as i64)
            }

            Command::SUnion(keys) => {
                let now = now_ms();
                match set_union(&self.data, &keys, now) {
                    Err(e) => e,
                    Ok(result) => set_to_value(result),
                }
            }

            Command::SUnionStore(dst, keys) => {
                let now = now_ms();
                let result = {
                    match set_union(&self.data, &keys, now) {
                        Err(e) => return e,
                        Ok(r) => r,
                    }
                };
                let len = result.len();
                self.data.insert(
                    dst,
                    Entry {
                        value: EntryValue::Set(result),
                        expires_at_ms: None,
                        last_access_ms: AtomicU64::new(now_ms()),
                    },
                );
                Value::Integer(len as i64)
            }

            Command::SDiff(keys) => {
                let now = now_ms();
                match set_diff(&self.data, &keys, now) {
                    Err(e) => e,
                    Ok(result) => set_to_value(result),
                }
            }

            Command::SDiffStore(dst, keys) => {
                let now = now_ms();
                let result = {
                    match set_diff(&self.data, &keys, now) {
                        Err(e) => return e,
                        Ok(r) => r,
                    }
                };
                let len = result.len();
                self.data.insert(
                    dst,
                    Entry {
                        value: EntryValue::Set(result),
                        expires_at_ms: None,
                        last_access_ms: AtomicU64::new(now_ms()),
                    },
                );
                Value::Integer(len as i64)
            }

            Command::SPop(key, count) => {
                let now = now_ms();
                match self.data.get_mut(&key) {
                    None => no_list_response(count),
                    Some(e) if e.is_expired(now) => no_list_response(count),
                    Some(mut e) => match &mut e.value {
                        EntryValue::Set(s) => {
                            let n = count.unwrap_or(1) as usize;
                            let mut rng = rand::rng();
                            // SPOP removes *random* members, not iteration-order ones.
                            // swap_remove_index is O(1), so popping k members costs
                            // O(k) regardless of set size.
                            let popped: Vec<String> = if n >= s.len() {
                                s.drain(..).collect()
                            } else {
                                (0..n)
                                    .map(|_| {
                                        let idx = rng.random_range(0..s.len());
                                        s.swap_remove_index(idx).expect("index in range")
                                    })
                                    .collect()
                            };
                            if count.is_some() {
                                Value::Array(Some(
                                    popped
                                        .into_iter()
                                        .map(|m| Value::BulkString(Some(m.into_bytes())))
                                        .collect(),
                                ))
                            } else {
                                popped
                                    .into_iter()
                                    .next()
                                    .map(|m| Value::BulkString(Some(m.into_bytes())))
                                    .unwrap_or(Value::BulkString(None))
                            }
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::SRandMember(key, count) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => match count {
                        None => Value::BulkString(None),
                        Some(_) => Value::Array(Some(vec![])),
                    },
                    Some(e) if e.is_expired(now) => match count {
                        None => Value::BulkString(None),
                        Some(_) => Value::Array(Some(vec![])),
                    },
                    Some(e) => match &e.value {
                        EntryValue::Set(s) => match count {
                            None => {
                                if s.is_empty() {
                                    return Value::BulkString(None);
                                }
                                let mut rng = rand::rng();
                                let idx = rng.random_range(0..s.len());
                                Value::BulkString(Some(s[idx].as_bytes().to_vec()))
                            }
                            Some(n) if n >= 0 => {
                                // Positive count: up to n *distinct* random members.
                                let mut rng = rand::rng();
                                let amount = (n as usize).min(s.len());
                                let idxs = rand::seq::index::sample(&mut rng, s.len(), amount);
                                Value::Array(Some(
                                    idxs.iter()
                                        .map(|i| Value::BulkString(Some(s[i].as_bytes().to_vec())))
                                        .collect(),
                                ))
                            }
                            Some(n) => {
                                // Negative: allow repetition, return |n| random elements.
                                if s.is_empty() {
                                    return Value::Array(Some(vec![]));
                                }
                                let mut rng = rand::rng();
                                let abs = n.unsigned_abs() as usize;
                                Value::Array(Some(
                                    (0..abs)
                                        .map(|_| {
                                            let idx = rng.random_range(0..s.len());
                                            Value::BulkString(Some(s[idx].as_bytes().to_vec()))
                                        })
                                        .collect(),
                                ))
                            }
                        },
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::SMove(src, dst, member) => {
                let now = now_ms();
                // Check types
                let src_type_ok = match self.data.get(&src) {
                    None => true,
                    Some(e) if e.is_expired(now) => true,
                    Some(e) => matches!(&e.value, EntryValue::Set(_)),
                };
                let dst_type_ok = match self.data.get(&dst) {
                    None => true,
                    Some(e) if e.is_expired(now) => true,
                    Some(e) => matches!(&e.value, EntryValue::Set(_)),
                };
                if !src_type_ok || !dst_type_ok {
                    return Value::Error(WRONGTYPE.to_string());
                }
                // Remove from source
                let removed = match self.data.get_mut(&src) {
                    Some(mut e) if !e.is_expired(now) => {
                        if let EntryValue::Set(s) = &mut e.value {
                            s.swap_remove(&member)
                        } else {
                            false
                        }
                    }
                    _ => false,
                };
                if !removed {
                    return Value::Integer(0);
                }
                // Add to destination
                let was_expired_dst = matches!(self.data.get(&dst), Some(e) if e.is_expired(now));
                let mut dst_entry = self.data.entry(dst).or_insert_with(|| Entry {
                    value: EntryValue::Set(IndexSet::new()),
                    expires_at_ms: None,
                    last_access_ms: AtomicU64::new(now_ms()),
                });
                if was_expired_dst {
                    dst_entry.value = EntryValue::Set(IndexSet::new());
                    dst_entry.expires_at_ms = None;
                }
                if let EntryValue::Set(s) = &mut dst_entry.value {
                    s.insert(member);
                }
                Value::Integer(1)
            }

            // ── Sorted Set ────────────────────────────────────────────────────
            Command::ZAdd(key, opts, pairs) => {
                let now = now_ms();
                let was_expired = type_guard!(self.data, &key, EntryValue::ZSet(_), now);
                let mut entry = self.data.entry(key).or_insert_with(|| Entry {
                    value: EntryValue::ZSet(ZSetInner::new()),
                    expires_at_ms: None,
                    last_access_ms: AtomicU64::new(now_ms()),
                });
                if was_expired {
                    entry.value = EntryValue::ZSet(ZSetInner::new());
                    entry.expires_at_ms = None;
                }
                let zset = match &mut entry.value {
                    EntryValue::ZSet(z) => z,
                    _ => unreachable!(),
                };
                zadd_exec(zset, opts, pairs)
            }

            Command::ZRange(key, start, stop, withscores) => zset_read(&self.data, &key, |zset| {
                let sorted = zset.rank_asc();
                Ok(encode_zrange(
                    zrange_index(&sorted, start, stop),
                    withscores,
                ))
            }),

            Command::ZRevRange(key, start, stop, withscores) => {
                zset_read(&self.data, &key, |zset| {
                    let mut sorted = zset.rank_asc();
                    sorted.reverse();
                    Ok(encode_zrange(
                        zrange_index(&sorted, start, stop),
                        withscores,
                    ))
                })
            }

            Command::ZRangeByScore(key, min_s, max_s, withscores, limit) => {
                zset_read(&self.data, &key, |zset| {
                    let min = ScoreBound::parse(&min_s)?;
                    let max = ScoreBound::parse(&max_s)?;
                    let filtered: Vec<(&str, f64)> = zset
                        .rank_asc()
                        .into_iter()
                        .filter(|(_, s)| in_score_range(*s, &min, &max))
                        .collect();
                    let limited = apply_limit(filtered, limit);
                    Ok(encode_zrange(&limited, withscores))
                })
            }

            Command::ZRevRangeByScore(key, max_s, min_s, withscores, limit) => {
                zset_read(&self.data, &key, |zset| {
                    let min = ScoreBound::parse(&min_s)?;
                    let max = ScoreBound::parse(&max_s)?;
                    let filtered: Vec<(&str, f64)> = {
                        let mut v: Vec<(&str, f64)> = zset
                            .rank_asc()
                            .into_iter()
                            .filter(|(_, s)| in_score_range(*s, &min, &max))
                            .collect();
                        v.reverse();
                        v
                    };
                    let limited = apply_limit(filtered, limit);
                    Ok(encode_zrange(&limited, withscores))
                })
            }

            Command::ZScore(key, member) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::BulkString(None),
                    Some(e) if e.is_expired(now) => Value::BulkString(None),
                    Some(e) => match &e.value {
                        EntryValue::ZSet(z) => {
                            e.touch(now);
                            z.scores
                                .get(&member)
                                .map(|s| Value::BulkString(Some(format_score(*s).into_bytes())))
                                .unwrap_or(Value::BulkString(None))
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::ZMScore(key, members) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Array(Some(
                        members.iter().map(|_| Value::BulkString(None)).collect(),
                    )),
                    Some(e) if e.is_expired(now) => Value::Array(Some(
                        members.iter().map(|_| Value::BulkString(None)).collect(),
                    )),
                    Some(e) => match &e.value {
                        EntryValue::ZSet(z) => Value::Array(Some(
                            members
                                .iter()
                                .map(|m| {
                                    z.scores
                                        .get(m)
                                        .map(|s| {
                                            Value::BulkString(Some(format_score(*s).into_bytes()))
                                        })
                                        .unwrap_or(Value::BulkString(None))
                                })
                                .collect(),
                        )),
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::ZRank(key, member) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::BulkString(None),
                    Some(e) if e.is_expired(now) => Value::BulkString(None),
                    Some(e) => match &e.value {
                        EntryValue::ZSet(z) => z
                            .rank_asc()
                            .iter()
                            .position(|(m, _)| *m == member)
                            .map(|i| Value::Integer(i as i64))
                            .unwrap_or(Value::BulkString(None)),
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::ZRevRank(key, member) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::BulkString(None),
                    Some(e) if e.is_expired(now) => Value::BulkString(None),
                    Some(e) => match &e.value {
                        EntryValue::ZSet(z) => {
                            let sorted = z.rank_asc();
                            let len = sorted.len();
                            sorted
                                .iter()
                                .position(|(m, _)| *m == member)
                                .map(|i| Value::Integer((len - 1 - i) as i64))
                                .unwrap_or(Value::BulkString(None))
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::ZRem(key, members) => {
                let now = now_ms();
                match self.data.get_mut(&key) {
                    None => Value::Integer(0),
                    Some(e) if e.is_expired(now) => Value::Integer(0),
                    Some(mut e) => match &mut e.value {
                        EntryValue::ZSet(z) => {
                            let removed = members
                                .iter()
                                .filter(|m| z.scores.remove(*m).is_some())
                                .count();
                            Value::Integer(removed as i64)
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::ZCard(key) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::Integer(0),
                    Some(e) if e.is_expired(now) => Value::Integer(0),
                    Some(e) => match &e.value {
                        EntryValue::ZSet(z) => Value::Integer(z.scores.len() as i64),
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::ZIncrBy(key, delta, member) => {
                let now = now_ms();
                let was_expired = type_guard!(self.data, &key, EntryValue::ZSet(_), now);
                let mut entry = self.data.entry(key).or_insert_with(|| Entry {
                    value: EntryValue::ZSet(ZSetInner::new()),
                    expires_at_ms: None,
                    last_access_ms: AtomicU64::new(now_ms()),
                });
                if was_expired {
                    entry.value = EntryValue::ZSet(ZSetInner::new());
                    entry.expires_at_ms = None;
                }
                let zset = match &mut entry.value {
                    EntryValue::ZSet(z) => z,
                    _ => unreachable!(),
                };
                let prev_score = zset.scores.get(&member).copied().unwrap_or(0.0);
                let new_score = prev_score + delta;
                if new_score.is_nan() || new_score.is_infinite() {
                    return Value::Error("ERR increment would produce NaN or Infinity".to_string());
                }
                zset.scores.insert(member, new_score);
                Value::BulkString(Some(format_score(new_score).into_bytes()))
            }

            Command::ZCount(key, min_s, max_s) => zset_read(&self.data, &key, |zset| {
                let min = ScoreBound::parse(&min_s)?;
                let max = ScoreBound::parse(&max_s)?;
                let count = zset
                    .scores
                    .values()
                    .filter(|&&s| in_score_range(s, &min, &max))
                    .count();
                Ok(Value::Integer(count as i64))
            }),

            // ── JSON ──────────────────────────────────────────────────────────
            Command::JSet(key, path, value) => {
                let now = now_ms();
                let was_expired = type_guard!(self.data, &key, EntryValue::Json(_), now);
                let segs = match parse_json_path(&path) {
                    Ok(s) => s,
                    Err(e) => return Value::Error(e),
                };
                let val: serde_json::Value = match serde_json::from_str(&value) {
                    Ok(v) => v,
                    Err(e) => return Value::Error(format!("ERR invalid JSON value: {}", e)),
                };
                // A fresh document starts as null; a leading index segment can
                // never apply to it — reject before creating the key.
                let is_fresh = was_expired || !self.data.contains_key(&key);
                if is_fresh && matches!(segs.first(), Some(JsonPathSeg::Index(_))) {
                    return Value::Error(
                        "ERR path segment '[..]' is not an array (key does not exist)".to_string(),
                    );
                }
                let mut entry = self.data.entry(key).or_insert_with(|| Entry {
                    value: EntryValue::Json(serde_json::Value::Null),
                    expires_at_ms: None,
                    last_access_ms: AtomicU64::new(now_ms()),
                });
                if was_expired {
                    entry.value = EntryValue::Json(serde_json::Value::Null);
                    entry.expires_at_ms = None;
                }
                let doc = match &mut entry.value {
                    EntryValue::Json(d) => d,
                    _ => unreachable!(),
                };
                match json_set_at(doc, &segs, val) {
                    Ok(()) => Value::SimpleString("OK".to_string()),
                    Err(e) => Value::Error(e),
                }
            }

            Command::JGet(key, path) => {
                let now = now_ms();
                match self.data.get(&key) {
                    None => Value::BulkString(None),
                    Some(e) if e.is_expired(now) => Value::BulkString(None),
                    Some(e) => match &e.value {
                        EntryValue::Json(doc) => {
                            e.touch(now);
                            let segs = match parse_json_path(path.as_deref().unwrap_or("$")) {
                                Ok(s) => s,
                                Err(err) => return Value::Error(err),
                            };
                            match json_get_at(doc, &segs) {
                                Some(v) => Value::BulkString(Some(
                                    serde_json::to_string(v).unwrap_or_default().into_bytes(),
                                )),
                                None => Value::BulkString(None),
                            }
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                }
            }

            Command::JMerge(key, patch) => {
                let now = now_ms();
                let was_expired = type_guard!(self.data, &key, EntryValue::Json(_), now);
                let patch: serde_json::Value = match serde_json::from_str(&patch) {
                    Ok(v) => v,
                    Err(e) => return Value::Error(format!("ERR invalid JSON patch: {}", e)),
                };
                if patch.is_null() {
                    // RFC 7386: a null patch replaces the target — the key is
                    // deleted rather than left holding a bare null.
                    self.data.remove(&key);
                    return Value::SimpleString("OK".to_string());
                }
                let mut entry = self.data.entry(key).or_insert_with(|| Entry {
                    value: EntryValue::Json(serde_json::Value::Null),
                    expires_at_ms: None,
                    last_access_ms: AtomicU64::new(now_ms()),
                });
                if was_expired {
                    entry.value = EntryValue::Json(serde_json::Value::Null);
                    entry.expires_at_ms = None;
                }
                let doc = match &mut entry.value {
                    EntryValue::Json(d) => d,
                    _ => unreachable!(),
                };
                json_merge_patch(doc, patch);
                Value::SimpleString("OK".to_string())
            }

            // ── Rate limiting ─────────────────────────────────────────────────
            Command::RlSet(key, limit, window_secs) => {
                let now = now_ms();
                let was_expired = type_guard!(self.data, &key, EntryValue::RateLimiter(_), now);
                let window_ms = window_secs.saturating_mul(1000);
                let mut entry = self.data.entry(key).or_insert_with(|| Entry {
                    value: EntryValue::RateLimiter(RateLimiterInner::new(limit, window_ms)),
                    expires_at_ms: None,
                    last_access_ms: AtomicU64::new(now_ms()),
                });
                if was_expired {
                    entry.value = EntryValue::RateLimiter(RateLimiterInner::new(limit, window_ms));
                }
                match &mut entry.value {
                    EntryValue::RateLimiter(rl) => {
                        // Reconfigure in place; recorded attempts are kept so a
                        // live limiter is not reset by a config change.
                        rl.limit = limit;
                        rl.window_ms = window_ms;
                    }
                    _ => unreachable!(),
                }
                // Explicitly configured limiters persist until DEL/EXPIRE, unlike
                // limiters auto-created by inline RLCHECK config.
                entry.expires_at_ms = None;
                Value::SimpleString("OK".to_string())
            }

            Command::RlCheck(key, config) => {
                let now = now_ms();
                let was_expired = type_guard!(self.data, &key, EntryValue::RateLimiter(_), now);
                let missing = was_expired
                    || match self.data.get(&key) {
                        None => true,
                        Some(e) => e.is_expired(now),
                    };
                if missing {
                    let Some((limit, window_secs)) = config else {
                        return Value::Error(format!(
                            "ERR no rate limit configured for '{}'; call RLSET first or use RLCHECK key limit window",
                            key
                        ));
                    };
                    let window_ms = window_secs.saturating_mul(1000);
                    // Auto-created limiters self-clean: they expire one window
                    // after the last attempt, so per-IP / per-user keys don't
                    // accumulate forever.
                    self.data.insert(
                        key.clone(),
                        Entry {
                            value: EntryValue::RateLimiter(RateLimiterInner::new(limit, window_ms)),
                            expires_at_ms: Some(now.saturating_add(window_ms)),
                            last_access_ms: AtomicU64::new(now),
                        },
                    );
                }
                match self.data.get_mut(&key) {
                    Some(mut e) => match &mut e.value {
                        EntryValue::RateLimiter(rl) => {
                            if let Some((limit, window_secs)) = config {
                                // Inline config wins: middleware config changes
                                // propagate without a separate RLSET.
                                rl.limit = limit;
                                rl.window_ms = window_secs.saturating_mul(1000);
                            }
                            let (allowed, remaining, retry_after_ms) = rl.check(now);
                            let window_ms = rl.window_ms;
                            if e.expires_at_ms.is_some() {
                                e.expires_at_ms = Some(now.saturating_add(window_ms));
                            }
                            Value::Array(Some(vec![
                                Value::Integer(allowed),
                                Value::Integer(remaining as i64),
                                Value::Integer(retry_after_ms as i64),
                            ]))
                        }
                        _ => Value::Error(WRONGTYPE.to_string()),
                    },
                    None => Value::Error("ERR rate limiter vanished mid-check".to_string()),
                }
            }

            // ── Transactions ─────────────────────────────────────────────────
            // These are handled at the server layer before reaching the store.
            // The arms below are fallback-only (e.g. store used in tests).
            Command::Multi => Value::SimpleString("OK".to_string()),
            Command::Exec => Value::Error("ERR EXEC without MULTI".to_string()),
            Command::Discard => Value::Error("ERR DISCARD without MULTI".to_string()),

            // ── Pub/Sub ───────────────────────────────────────────────────────
            // Routing is handled entirely in the server layer.
            Command::Subscribe(_)
            | Command::Unsubscribe(_)
            | Command::PSubscribe(_)
            | Command::PUnsubscribe(_) => Value::Error("ERR only in pub/sub context".to_string()),
            // Sync scoping is a WebSocket-connection concern, handled entirely
            // in the server layer.
            Command::Sync(_) => {
                Value::Error("ERR SYNC is only available on the WebSocket port".to_string())
            }
            // Live queries are likewise per-WebSocket-connection state.
            Command::QSub(_) | Command::QUnsub(_) => Value::Error(
                "ERR live queries are only available on the WebSocket port".to_string(),
            ),
            // Deduplication is unwrapped in the server layer before execution.
            Command::Dedup(_, _, _) => {
                Value::Error("ERR DEDUP is only available on the WebSocket port".to_string())
            }
            Command::Publish(_, _) => Value::Integer(0),

            Command::Unknown(name) => Value::Error(format!("ERR unknown command '{}'", name)),
            Command::Watch(_) | Command::Unwatch(_) => {
                Value::Error("ERR WATCH/UNWATCH only supported over WebSocket".to_string())
            }
            Command::Save | Command::BgSave | Command::LastSave => {
                Value::Error("ERR persistence commands must be handled by the server".to_string())
            }
            Command::ReplicaOfNoOne => {
                Value::Error("ERR REPLICAOF NO ONE must be handled by the server".to_string())
            }
        }
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// Approximate heap footprint of a single entry: key + value bytes plus a fixed
/// per-entry overhead. Shared by `approximate_memory_bytes` and the eviction
/// loop so both agree on what a key "costs".
fn entry_size(key: &str, e: &Entry) -> usize {
    let val_size = match &e.value {
        EntryValue::Str(s) => s.len(),
        EntryValue::Hash(m) => m.iter().map(|(k, v)| k.len() + v.len()).sum(),
        EntryValue::List(l) => l.iter().map(|s| s.len()).sum(),
        EntryValue::Set(s) => s.iter().map(|m| m.len()).sum::<usize>(),
        EntryValue::ZSet(z) => z.scores.keys().map(|m| m.len() + 8).sum(),
        EntryValue::RateLimiter(rl) => rl.events.len() * 8 + 16,
        EntryValue::Json(doc) => json_approx_size(doc),
    };
    key.len() + val_size + 64
}

fn incr_by(data: &DashMap<String, Entry>, key: String, delta: i64) -> Value {
    let now = now_ms();
    let was_expired = match data.get(&key) {
        None => false,
        Some(e) if e.is_expired(now) => true,
        Some(e) => match &e.value {
            EntryValue::Str(_) => false,
            _ => return Value::Error(WRONGTYPE.to_string()),
        },
    };
    let mut entry = data
        .entry(key)
        .or_insert_with(|| Entry::new_str("0".to_string()));
    if was_expired {
        entry.value = EntryValue::Str("0".to_string());
        entry.expires_at_ms = None;
    }
    match &mut entry.value {
        EntryValue::Str(s) => match s.parse::<i64>() {
            Err(_) => Value::Error("ERR value is not an integer or out of range".to_string()),
            Ok(n) => match n.checked_add(delta) {
                None => Value::Error("ERR increment or decrement would overflow".to_string()),
                Some(new) => {
                    *s = new.to_string();
                    Value::Integer(new)
                }
            },
        },
        _ => unreachable!(),
    }
}

fn set_expiry(data: &DashMap<String, Entry>, key: String, ts_ms: u64) -> Value {
    let now = now_ms();
    match data.get_mut(&key) {
        None => Value::Integer(0),
        Some(e) if e.is_expired(now) => Value::Integer(0),
        Some(mut e) => {
            e.expires_at_ms = Some(ts_ms);
            Value::Integer(1)
        }
    }
}

/// Glob match with `*`, `?`, and `[abc]` classes — iterative DP, O(m × n),
/// immune to backtracking blowup. Used by KEYS/SCAN and exported for the
/// server layer's sync-scope filtering.
pub fn glob_match(pattern: &str, s: &str) -> bool {
    let pat = pattern.as_bytes();
    let text = s.as_bytes();
    let (m, n) = (pat.len(), text.len());

    // Iterative DP: prev[j] = pat[..i] matches text[..j].
    // This replaces a recursive matcher that had exponential worst-case
    // backtracking on patterns like "*.*.*x" against long non-matching strings.
    let mut prev = vec![false; n + 1];
    let mut curr = vec![false; n + 1];
    prev[0] = true;

    for i in 1..=m {
        curr[0] = pat[i - 1] == b'*' && prev[0];
        for j in 1..=n {
            curr[j] = if pat[i - 1] == b'*' {
                prev[j] || curr[j - 1]
            } else if pat[i - 1] == b'?' || pat[i - 1] == text[j - 1] {
                prev[j - 1]
            } else {
                false
            };
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

fn no_list_response(count: Option<u64>) -> Value {
    if count.is_some() {
        Value::Array(Some(vec![]))
    } else {
        Value::BulkString(None)
    }
}

fn set_to_value(mut result: IndexSet<String>) -> Value {
    let mut members: Vec<String> = result.drain(..).collect();
    members.sort_unstable();
    Value::Array(Some(
        members
            .into_iter()
            .map(|m| Value::BulkString(Some(m.into_bytes())))
            .collect(),
    ))
}

fn set_inter(
    data: &DashMap<String, Entry>,
    keys: &[String],
    now: u64,
) -> Result<IndexSet<String>, Value> {
    if keys.is_empty() {
        return Ok(IndexSet::new());
    }
    let mut sets: Vec<Option<IndexSet<String>>> = Vec::with_capacity(keys.len());
    for k in keys {
        let cloned = {
            let entry = data.get(k);
            match entry {
                None => None,
                Some(e) if e.is_expired(now) => None,
                Some(e) => match &e.value {
                    EntryValue::Set(s) => Some(s.clone()),
                    _ => return Err(Value::Error(WRONGTYPE.to_string())),
                },
            }
        };
        sets.push(cloned);
    }
    if sets.iter().any(|s| s.is_none()) {
        return Ok(IndexSet::new());
    }
    let non_empty: Vec<IndexSet<String>> = sets.into_iter().flatten().collect();
    let mut result: IndexSet<String> = non_empty[0].iter().cloned().collect();
    for s in &non_empty[1..] {
        result.retain(|m| s.contains(m));
    }
    Ok(result)
}

fn set_union(
    data: &DashMap<String, Entry>,
    keys: &[String],
    now: u64,
) -> Result<IndexSet<String>, Value> {
    let mut result: IndexSet<String> = IndexSet::new();
    for k in keys {
        let s_clone = {
            let entry = data.get(k);
            match entry {
                None => None,
                Some(e) if e.is_expired(now) => None,
                Some(e) => match &e.value {
                    EntryValue::Set(s) => Some(s.clone()),
                    _ => return Err(Value::Error(WRONGTYPE.to_string())),
                },
            }
        };
        if let Some(s) = s_clone {
            result.extend(s);
        }
    }
    Ok(result)
}

fn set_diff(
    data: &DashMap<String, Entry>,
    keys: &[String],
    now: u64,
) -> Result<IndexSet<String>, Value> {
    if keys.is_empty() {
        return Ok(IndexSet::new());
    }
    let mut result: IndexSet<String> = match data.get(&keys[0]) {
        None => IndexSet::new(),
        Some(e) if e.is_expired(now) => IndexSet::new(),
        Some(e) => match &e.value {
            EntryValue::Set(s) => s.iter().cloned().collect(),
            _ => return Err(Value::Error(WRONGTYPE.to_string())),
        },
    };
    for k in &keys[1..] {
        let s_clone = {
            let entry = data.get(k);
            match entry {
                None => None,
                Some(e) if e.is_expired(now) => None,
                Some(e) => match &e.value {
                    EntryValue::Set(s) => Some(s.clone()),
                    _ => return Err(Value::Error(WRONGTYPE.to_string())),
                },
            }
        };
        if let Some(s) = s_clone {
            result.retain(|m| !s.contains(m));
        }
    }
    Ok(result)
}

fn hash_incr_int(data: &DashMap<String, Entry>, key: String, field: String, delta: i64) -> Value {
    let now = now_ms();
    let was_expired = match data.get(&key) {
        None => false,
        Some(e) if e.is_expired(now) => true,
        Some(e) => match &e.value {
            EntryValue::Hash(_) => false,
            _ => return Value::Error(WRONGTYPE.to_string()),
        },
    };
    let mut entry = data.entry(key).or_insert_with(|| Entry {
        value: EntryValue::Hash(HashMap::new()),
        expires_at_ms: None,
        last_access_ms: AtomicU64::new(now_ms()),
    });
    if was_expired {
        entry.value = EntryValue::Hash(HashMap::new());
        entry.expires_at_ms = None;
    }
    let h = match &mut entry.value {
        EntryValue::Hash(h) => h,
        _ => unreachable!(),
    };
    let cur: i64 = h.get(&field).and_then(|s| s.parse().ok()).unwrap_or(0);
    match cur.checked_add(delta) {
        None => Value::Error("ERR increment or decrement would overflow".to_string()),
        Some(new) => {
            h.insert(field, new.to_string());
            Value::Integer(new)
        }
    }
}

fn hash_incr_float(data: &DashMap<String, Entry>, key: String, field: String, delta: f64) -> Value {
    let now = now_ms();
    let was_expired = match data.get(&key) {
        None => false,
        Some(e) if e.is_expired(now) => true,
        Some(e) => match &e.value {
            EntryValue::Hash(_) => false,
            _ => return Value::Error(WRONGTYPE.to_string()),
        },
    };
    let mut entry = data.entry(key).or_insert_with(|| Entry {
        value: EntryValue::Hash(HashMap::new()),
        expires_at_ms: None,
        last_access_ms: AtomicU64::new(now_ms()),
    });
    if was_expired {
        entry.value = EntryValue::Hash(HashMap::new());
        entry.expires_at_ms = None;
    }
    let h = match &mut entry.value {
        EntryValue::Hash(h) => h,
        _ => unreachable!(),
    };
    let cur: f64 = h.get(&field).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let new = cur + delta;
    if new.is_nan() || new.is_infinite() {
        return Value::Error("ERR increment would produce NaN or Infinity".to_string());
    }
    let new_str = format_score(new);
    h.insert(field, new_str.clone());
    Value::BulkString(Some(new_str.into_bytes()))
}

fn zset_read<F>(data: &DashMap<String, Entry>, key: &str, f: F) -> Value
where
    F: FnOnce(&ZSetInner) -> Result<Value, Value>,
{
    let now = now_ms();
    let empty = ZSetInner::new();
    let result = match data.get(key) {
        None => f(&empty),
        Some(e) if e.is_expired(now) => f(&empty),
        Some(e) => match &e.value {
            EntryValue::ZSet(z) => {
                e.touch(now);
                f(z)
            }
            _ => return Value::Error(WRONGTYPE.to_string()),
        },
    };
    match result {
        Ok(v) | Err(v) => v,
    }
}

fn zadd_exec(zset: &mut ZSetInner, opts: ZAddOptions, pairs: Vec<(f64, String)>) -> Value {
    use crate::cmd::ZAddCondition;

    if opts.incr {
        let (delta, member) = match pairs.into_iter().next() {
            Some(p) => p,
            None => return Value::BulkString(None),
        };
        let score = zset
            .scores
            .entry(member)
            .and_modify(|s| *s += delta)
            .or_insert(delta);
        return Value::BulkString(Some(format_score(*score).into_bytes()));
    }

    let mut added = 0i64;
    let mut changed = 0i64;
    for (score, member) in pairs {
        match &opts.condition {
            Some(ZAddCondition::Nx) => {
                if let std::collections::hash_map::Entry::Vacant(e) = zset.scores.entry(member) {
                    e.insert(score);
                    added += 1;
                    changed += 1;
                }
            }
            Some(ZAddCondition::Xx) => {
                if let Some(old_score) = zset.scores.get_mut(&member) {
                    let should_update = if opts.gt {
                        score > *old_score
                    } else if opts.lt {
                        score < *old_score
                    } else {
                        (score - *old_score).abs() > f64::EPSILON
                    };
                    if should_update {
                        *old_score = score;
                        changed += 1;
                    }
                }
            }
            None => {
                if opts.gt || opts.lt {
                    match zset.scores.entry(member) {
                        std::collections::hash_map::Entry::Vacant(e) => {
                            e.insert(score);
                            added += 1;
                            changed += 1;
                        }
                        std::collections::hash_map::Entry::Occupied(mut e) => {
                            let old_score = *e.get();
                            let should_update = if opts.gt {
                                score > old_score
                            } else {
                                score < old_score
                            };
                            if should_update {
                                e.insert(score);
                                changed += 1;
                            }
                        }
                    }
                } else {
                    let old = zset.scores.insert(member, score);
                    match old {
                        None => {
                            added += 1;
                            changed += 1;
                        }
                        Some(old_score) if (old_score - score).abs() > f64::EPSILON => {
                            changed += 1;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Value::Integer(if opts.ch { changed } else { added })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::{Command, SetOptions, ZAddOptions};

    fn store() -> KeyValueStore {
        KeyValueStore::new()
    }

    fn bulk(s: &str) -> Value {
        Value::BulkString(Some(s.as_bytes().to_vec()))
    }

    fn int(n: i64) -> Value {
        Value::Integer(n)
    }

    fn ok() -> Value {
        Value::SimpleString("OK".to_string())
    }

    fn nil() -> Value {
        Value::BulkString(None)
    }

    fn arr(items: &[&str]) -> Value {
        Value::Array(Some(items.iter().map(|s| bulk(s)).collect()))
    }

    // ── Hash ──────────────────────────────────────────────────────────────────

    #[test]
    fn hash_basic() {
        let s = store();
        assert_eq!(
            s.execute(Command::HSet(
                "h".into(),
                vec![("f1".into(), "v1".into()), ("f2".into(), "v2".into())]
            )),
            int(2)
        );
        assert_eq!(
            s.execute(Command::HGet("h".into(), "f1".into())),
            bulk("v1")
        );
        assert_eq!(
            s.execute(Command::HGet("h".into(), "f2".into())),
            bulk("v2")
        );
        assert_eq!(s.execute(Command::HGet("h".into(), "nope".into())), nil());
        assert_eq!(s.execute(Command::HLen("h".into())), int(2));
    }

    #[test]
    fn hash_getall_sorted() {
        let s = store();
        s.execute(Command::HSet(
            "h".into(),
            vec![("b".into(), "2".into()), ("a".into(), "1".into())],
        ));
        // HGETALL returns field-value pairs sorted by field
        let res = s.execute(Command::HGetAll("h".into()));
        assert_eq!(res, arr(&["a", "1", "b", "2"]));
    }

    #[test]
    fn hash_del() {
        let s = store();
        s.execute(Command::HSet("h".into(), vec![("f".into(), "v".into())]));
        assert_eq!(
            s.execute(Command::HDel("h".into(), vec!["f".into()])),
            int(1)
        );
        assert_eq!(
            s.execute(Command::HDel("h".into(), vec!["f".into()])),
            int(0)
        );
        assert_eq!(s.execute(Command::HGet("h".into(), "f".into())), nil());
    }

    #[test]
    fn hash_incr() {
        let s = store();
        assert_eq!(
            s.execute(Command::HIncrBy("h".into(), "n".into(), 5)),
            int(5)
        );
        assert_eq!(
            s.execute(Command::HIncrBy("h".into(), "n".into(), 3)),
            int(8)
        );
        let res = s.execute(Command::HIncrByFloat("h".into(), "f".into(), 1.5));
        assert_eq!(res, bulk("1.5"));
    }

    #[test]
    fn hash_hsetnx() {
        let s = store();
        assert_eq!(
            s.execute(Command::HSetNx("h".into(), "f".into(), "v1".into())),
            int(1)
        );
        assert_eq!(
            s.execute(Command::HSetNx("h".into(), "f".into(), "v2".into())),
            int(0)
        );
        assert_eq!(s.execute(Command::HGet("h".into(), "f".into())), bulk("v1"));
    }

    #[test]
    fn hash_hmget() {
        let s = store();
        s.execute(Command::HSet("h".into(), vec![("a".into(), "1".into())]));
        let res = s.execute(Command::HMGet("h".into(), vec!["a".into(), "b".into()]));
        assert_eq!(res, Value::Array(Some(vec![bulk("1"), nil()])));
    }

    #[test]
    fn hash_wrongtype() {
        let s = store();
        s.execute(Command::Set("k".into(), "v".into(), SetOptions::default()));
        let res = s.execute(Command::HGet("k".into(), "f".into()));
        assert!(matches!(res, Value::Error(e) if e.contains("WRONGTYPE")));
    }

    // ── List ─────────────────────────────────────────────────────────────────

    #[test]
    fn list_push_pop() {
        let s = store();
        assert_eq!(
            s.execute(Command::RPush("l".into(), vec!["a".into(), "b".into()])),
            int(2)
        );
        assert_eq!(
            s.execute(Command::LPush("l".into(), vec!["z".into()])),
            int(3)
        );
        // list is now: z a b
        assert_eq!(s.execute(Command::LPop("l".into(), None)), bulk("z"));
        assert_eq!(s.execute(Command::RPop("l".into(), None)), bulk("b"));
        assert_eq!(s.execute(Command::LLen("l".into())), int(1));
    }

    #[test]
    fn list_lrange() {
        let s = store();
        s.execute(Command::RPush(
            "l".into(),
            vec!["a".into(), "b".into(), "c".into()],
        ));
        assert_eq!(
            s.execute(Command::LRange("l".into(), 0, -1)),
            arr(&["a", "b", "c"])
        );
        assert_eq!(
            s.execute(Command::LRange("l".into(), 1, 2)),
            arr(&["b", "c"])
        );
        assert_eq!(s.execute(Command::LRange("l".into(), 0, 0)), arr(&["a"]));
    }

    #[test]
    fn list_lindex_lset() {
        let s = store();
        s.execute(Command::RPush("l".into(), vec!["a".into(), "b".into()]));
        assert_eq!(s.execute(Command::LIndex("l".into(), 0)), bulk("a"));
        assert_eq!(s.execute(Command::LIndex("l".into(), -1)), bulk("b"));
        assert_eq!(s.execute(Command::LSet("l".into(), 0, "x".into())), ok());
        assert_eq!(s.execute(Command::LIndex("l".into(), 0)), bulk("x"));
    }

    #[test]
    fn list_lrem() {
        let s = store();
        s.execute(Command::RPush(
            "l".into(),
            vec!["a".into(), "b".into(), "a".into(), "c".into()],
        ));
        assert_eq!(s.execute(Command::LRem("l".into(), 1, "a".into())), int(1));
        assert_eq!(
            s.execute(Command::LRange("l".into(), 0, -1)),
            arr(&["b", "a", "c"])
        );
    }

    #[test]
    fn list_ltrim() {
        let s = store();
        s.execute(Command::RPush(
            "l".into(),
            vec!["a".into(), "b".into(), "c".into()],
        ));
        s.execute(Command::LTrim("l".into(), 1, 2));
        assert_eq!(
            s.execute(Command::LRange("l".into(), 0, -1)),
            arr(&["b", "c"])
        );
    }

    #[test]
    fn list_wrongtype() {
        let s = store();
        s.execute(Command::Set("k".into(), "v".into(), SetOptions::default()));
        let res = s.execute(Command::LPush("k".into(), vec!["x".into()]));
        assert!(matches!(res, Value::Error(e) if e.contains("WRONGTYPE")));
    }

    // ── Set ───────────────────────────────────────────────────────────────────

    #[test]
    fn set_basic() {
        let s = store();
        assert_eq!(
            s.execute(Command::SAdd("s".into(), vec!["a".into(), "b".into()])),
            int(2)
        );
        assert_eq!(
            s.execute(Command::SAdd("s".into(), vec!["a".into()])),
            int(0)
        );
        assert_eq!(s.execute(Command::SCard("s".into())), int(2));
        assert_eq!(
            s.execute(Command::SIsMember("s".into(), "a".into())),
            int(1)
        );
        assert_eq!(
            s.execute(Command::SIsMember("s".into(), "z".into())),
            int(0)
        );
    }

    #[test]
    fn set_smembers_sorted() {
        let s = store();
        s.execute(Command::SAdd(
            "s".into(),
            vec!["c".into(), "a".into(), "b".into()],
        ));
        assert_eq!(
            s.execute(Command::SMembers("s".into())),
            arr(&["a", "b", "c"])
        );
    }

    #[test]
    fn set_rem() {
        let s = store();
        s.execute(Command::SAdd("s".into(), vec!["a".into(), "b".into()]));
        assert_eq!(
            s.execute(Command::SRem("s".into(), vec!["a".into()])),
            int(1)
        );
        assert_eq!(s.execute(Command::SCard("s".into())), int(1));
    }

    #[test]
    fn set_inter_union_diff() {
        let s = store();
        s.execute(Command::SAdd(
            "a".into(),
            vec!["1".into(), "2".into(), "3".into()],
        ));
        s.execute(Command::SAdd(
            "b".into(),
            vec!["2".into(), "3".into(), "4".into()],
        ));

        let inter = s.execute(Command::SInter(vec!["a".into(), "b".into()]));
        assert_eq!(inter, arr(&["2", "3"]));

        let union = s.execute(Command::SUnion(vec!["a".into(), "b".into()]));
        assert_eq!(union, arr(&["1", "2", "3", "4"]));

        let diff = s.execute(Command::SDiff(vec!["a".into(), "b".into()]));
        assert_eq!(diff, arr(&["1"]));
    }

    #[test]
    fn set_smove() {
        let s = store();
        s.execute(Command::SAdd("src".into(), vec!["m".into()]));
        assert_eq!(
            s.execute(Command::SMove("src".into(), "dst".into(), "m".into())),
            int(1)
        );
        assert_eq!(
            s.execute(Command::SIsMember("src".into(), "m".into())),
            int(0)
        );
        assert_eq!(
            s.execute(Command::SIsMember("dst".into(), "m".into())),
            int(1)
        );
    }

    #[test]
    fn set_wrongtype() {
        let s = store();
        s.execute(Command::Set("k".into(), "v".into(), SetOptions::default()));
        let res = s.execute(Command::SAdd("k".into(), vec!["x".into()]));
        assert!(matches!(res, Value::Error(e) if e.contains("WRONGTYPE")));
    }

    // ── Sorted Set ────────────────────────────────────────────────────────────

    #[test]
    fn zset_zadd_zrange() {
        let s = store();
        assert_eq!(
            s.execute(Command::ZAdd(
                "z".into(),
                ZAddOptions::default(),
                vec![(1.0, "a".into()), (2.0, "b".into())]
            )),
            int(2)
        );
        assert_eq!(
            s.execute(Command::ZRange("z".into(), 0, -1, false)),
            arr(&["a", "b"])
        );
        assert_eq!(
            s.execute(Command::ZRevRange("z".into(), 0, -1, false)),
            arr(&["b", "a"])
        );
    }

    #[test]
    fn zset_withscores() {
        let s = store();
        s.execute(Command::ZAdd(
            "z".into(),
            ZAddOptions::default(),
            vec![(1.0, "a".into())],
        ));
        let res = s.execute(Command::ZRange("z".into(), 0, -1, true));
        assert_eq!(res, arr(&["a", "1"]));
    }

    #[test]
    fn zset_zscore_zrank() {
        let s = store();
        s.execute(Command::ZAdd(
            "z".into(),
            ZAddOptions::default(),
            vec![(5.0, "a".into()), (3.0, "b".into())],
        ));
        assert_eq!(
            s.execute(Command::ZScore("z".into(), "a".into())),
            bulk("5")
        );
        assert_eq!(s.execute(Command::ZRank("z".into(), "b".into())), int(0));
        assert_eq!(s.execute(Command::ZRevRank("z".into(), "b".into())), int(1));
    }

    #[test]
    fn zset_zincrby() {
        let s = store();
        s.execute(Command::ZAdd(
            "z".into(),
            ZAddOptions::default(),
            vec![(1.0, "m".into())],
        ));
        assert_eq!(
            s.execute(Command::ZIncrBy("z".into(), 2.5, "m".into())),
            bulk("3.5")
        );
        assert_eq!(
            s.execute(Command::ZScore("z".into(), "m".into())),
            bulk("3.5")
        );
        // New member
        assert_eq!(
            s.execute(Command::ZIncrBy("z".into(), 10.0, "new".into())),
            bulk("10")
        );
    }

    #[test]
    fn zset_zrem_zcard() {
        let s = store();
        s.execute(Command::ZAdd(
            "z".into(),
            ZAddOptions::default(),
            vec![(1.0, "a".into()), (2.0, "b".into())],
        ));
        assert_eq!(
            s.execute(Command::ZRem("z".into(), vec!["a".into()])),
            int(1)
        );
        assert_eq!(s.execute(Command::ZCard("z".into())), int(1));
    }

    #[test]
    fn zset_zrangebyscore() {
        let s = store();
        s.execute(Command::ZAdd(
            "z".into(),
            ZAddOptions::default(),
            vec![(1.0, "a".into()), (2.0, "b".into()), (3.0, "c".into())],
        ));
        assert_eq!(
            s.execute(Command::ZRangeByScore(
                "z".into(),
                "1".into(),
                "2".into(),
                false,
                None
            )),
            arr(&["a", "b"])
        );
        assert_eq!(
            s.execute(Command::ZRangeByScore(
                "z".into(),
                "(1".into(),
                "+inf".into(),
                false,
                None
            )),
            arr(&["b", "c"])
        );
        assert_eq!(
            s.execute(Command::ZCount("z".into(), "-inf".into(), "2".into())),
            int(2)
        );
    }

    #[test]
    fn zset_zadd_nx_xx() {
        let s = store();
        s.execute(Command::ZAdd(
            "z".into(),
            ZAddOptions::default(),
            vec![(1.0, "m".into())],
        ));
        // NX: don't update existing
        s.execute(Command::ZAdd(
            "z".into(),
            ZAddOptions {
                condition: Some(crate::cmd::ZAddCondition::Nx),
                ..Default::default()
            },
            vec![(99.0, "m".into())],
        ));
        assert_eq!(
            s.execute(Command::ZScore("z".into(), "m".into())),
            bulk("1")
        );
        // XX: update existing only
        s.execute(Command::ZAdd(
            "z".into(),
            ZAddOptions {
                condition: Some(crate::cmd::ZAddCondition::Xx),
                ..Default::default()
            },
            vec![(5.0, "m".into()), (5.0, "new".into())],
        ));
        assert_eq!(
            s.execute(Command::ZScore("z".into(), "m".into())),
            bulk("5")
        );
        assert_eq!(s.execute(Command::ZScore("z".into(), "new".into())), nil());
    }

    #[test]
    fn zset_wrongtype() {
        let s = store();
        s.execute(Command::Set("k".into(), "v".into(), SetOptions::default()));
        let res = s.execute(Command::ZAdd(
            "k".into(),
            ZAddOptions::default(),
            vec![(1.0, "m".into())],
        ));
        assert!(matches!(res, Value::Error(e) if e.contains("WRONGTYPE")));
    }

    // ── Cross-type TTL ────────────────────────────────────────────────────────

    #[test]
    fn collection_ttl_expire() {
        let s = store();
        s.execute(Command::HSet("h".into(), vec![("f".into(), "v".into())]));
        assert_eq!(s.execute(Command::Expire("h".into(), 60)), int(1));
        assert_eq!(s.execute(Command::HGet("h".into(), "f".into())), bulk("v"));
        // Force expiry by manipulating via PEXPIRE with 0ms
        s.execute(Command::PExpire("h".into(), 0));
        // Now lazy-expired
        assert_eq!(s.execute(Command::HGet("h".into(), "f".into())), nil());
    }

    // ── Transactions (store-layer stubs) ──────────────────────────────────────

    #[test]
    fn multi_returns_ok_stub() {
        // Store-layer stub: server intercepts MULTI before execute(), but the
        // stub must return OK so the exhaustiveness arm is exercised here.
        let s = store();
        assert_eq!(s.execute(Command::Multi), ok());
    }

    #[test]
    fn exec_without_multi_error() {
        let s = store();
        let res = s.execute(Command::Exec);
        assert!(matches!(res, Value::Error(e) if e.contains("EXEC without MULTI")));
    }

    #[test]
    fn discard_without_multi_error() {
        let s = store();
        let res = s.execute(Command::Discard);
        assert!(matches!(res, Value::Error(e) if e.contains("DISCARD without MULTI")));
    }

    #[test]
    fn publish_stub_returns_zero() {
        let s = store();
        assert_eq!(
            s.execute(Command::Publish("ch".into(), "msg".into())),
            int(0)
        );
    }

    // ── Strings ───────────────────────────────────────────────────────────────

    #[test]
    fn string_get_missing_returns_nil() {
        let s = store();
        assert_eq!(s.execute(Command::Get("no_such_key".into())), nil());
    }

    #[test]
    fn string_append_and_strlen() {
        let s = store();
        s.execute(Command::Set(
            "k".into(),
            "hello".into(),
            SetOptions::default(),
        ));
        assert_eq!(
            s.execute(Command::Append("k".into(), " world".into())),
            int(11)
        );
        assert_eq!(s.execute(Command::Strlen("k".into())), int(11));
        assert_eq!(s.execute(Command::Get("k".into())), bulk("hello world"));
        // APPEND on missing key creates it
        assert_eq!(
            s.execute(Command::Append("new".into(), "abc".into())),
            int(3)
        );
        assert_eq!(s.execute(Command::Strlen("new".into())), int(3));
    }

    #[test]
    fn string_getset() {
        let s = store();
        s.execute(Command::Set(
            "k".into(),
            "old".into(),
            SetOptions::default(),
        ));
        assert_eq!(
            s.execute(Command::GetSet("k".into(), "new".into())),
            bulk("old")
        );
        assert_eq!(s.execute(Command::Get("k".into())), bulk("new"));
        // GETSET on missing key returns nil
        assert_eq!(
            s.execute(Command::GetSet("missing".into(), "v".into())),
            nil()
        );
    }

    #[test]
    fn string_incr_decr() {
        let s = store();
        assert_eq!(s.execute(Command::Incr("n".into())), int(1));
        assert_eq!(s.execute(Command::Incr("n".into())), int(2));
        assert_eq!(s.execute(Command::Decr("n".into())), int(1));
        assert_eq!(s.execute(Command::Decr("n".into())), int(0));
        assert_eq!(s.execute(Command::Decr("n".into())), int(-1));
    }

    #[test]
    fn string_incrby_decrby() {
        let s = store();
        assert_eq!(s.execute(Command::IncrBy("n".into(), 10)), int(10));
        assert_eq!(s.execute(Command::IncrBy("n".into(), 5)), int(15));
        assert_eq!(s.execute(Command::DecrBy("n".into(), 3)), int(12));
        assert_eq!(s.execute(Command::DecrBy("n".into(), 20)), int(-8));
    }

    #[test]
    fn string_mset_mget() {
        let s = store();
        assert_eq!(
            s.execute(Command::MSet(vec![
                ("a".into(), "1".into()),
                ("b".into(), "2".into()),
            ])),
            ok()
        );
        let res = s.execute(Command::MGet(vec![
            "a".into(),
            "b".into(),
            "missing".into(),
        ]));
        assert_eq!(res, Value::Array(Some(vec![bulk("1"), bulk("2"), nil()])));
    }

    #[test]
    fn string_setnx() {
        let s = store();
        assert_eq!(s.execute(Command::SetNx("k".into(), "v1".into())), int(1));
        assert_eq!(s.execute(Command::SetNx("k".into(), "v2".into())), int(0));
        assert_eq!(s.execute(Command::Get("k".into())), bulk("v1"));
    }

    #[test]
    fn string_setex() {
        let s = store();
        assert_eq!(s.execute(Command::SetEx("k".into(), 60, "v".into())), ok());
        assert_eq!(s.execute(Command::Get("k".into())), bulk("v"));
        // TTL should be positive
        let ttl = s.execute(Command::Ttl("k".into()));
        assert!(matches!(ttl, Value::Integer(n) if n > 0 && n <= 60));
    }

    #[test]
    fn string_del_multi_key() {
        let s = store();
        s.execute(Command::MSet(vec![
            ("a".into(), "1".into()),
            ("b".into(), "2".into()),
            ("c".into(), "3".into()),
        ]));
        // DEL returns count of deleted keys
        assert_eq!(
            s.execute(Command::Del(vec!["a".into(), "b".into(), "ghost".into()])),
            int(2)
        );
        assert_eq!(s.execute(Command::Get("a".into())), nil());
        assert_eq!(s.execute(Command::Get("c".into())), bulk("3"));
    }

    #[test]
    fn string_unlink_behaves_like_del() {
        let s = store();
        s.execute(Command::Set("k".into(), "v".into(), SetOptions::default()));
        assert_eq!(s.execute(Command::Unlink(vec!["k".into()])), int(1));
        assert_eq!(s.execute(Command::Get("k".into())), nil());
    }

    // ── Keys ──────────────────────────────────────────────────────────────────

    #[test]
    fn keys_exists_single_and_multi() {
        let s = store();
        s.execute(Command::Set("a".into(), "1".into(), SetOptions::default()));
        s.execute(Command::Set("b".into(), "2".into(), SetOptions::default()));
        assert_eq!(s.execute(Command::Exists(vec!["a".into()])), int(1));
        assert_eq!(s.execute(Command::Exists(vec!["ghost".into()])), int(0));
        // EXISTS counts duplicates
        assert_eq!(
            s.execute(Command::Exists(vec!["a".into(), "b".into(), "a".into()])),
            int(3)
        );
    }

    #[test]
    fn keys_type_reports_correct_type() {
        let s = store();
        s.execute(Command::Set(
            "str".into(),
            "v".into(),
            SetOptions::default(),
        ));
        s.execute(Command::HSet("hsh".into(), vec![("f".into(), "v".into())]));
        s.execute(Command::LPush("lst".into(), vec!["x".into()]));
        s.execute(Command::SAdd("st".into(), vec!["x".into()]));
        s.execute(Command::ZAdd(
            "zst".into(),
            ZAddOptions::default(),
            vec![(1.0, "m".into())],
        ));

        assert_eq!(
            s.execute(Command::Type("str".into())),
            Value::SimpleString("string".into())
        );
        assert_eq!(
            s.execute(Command::Type("hsh".into())),
            Value::SimpleString("hash".into())
        );
        assert_eq!(
            s.execute(Command::Type("lst".into())),
            Value::SimpleString("list".into())
        );
        assert_eq!(
            s.execute(Command::Type("st".into())),
            Value::SimpleString("set".into())
        );
        assert_eq!(
            s.execute(Command::Type("zst".into())),
            Value::SimpleString("zset".into())
        );
        assert_eq!(
            s.execute(Command::Type("none".into())),
            Value::SimpleString("none".into())
        );
    }

    #[test]
    fn keys_rename() {
        let s = store();
        s.execute(Command::Set(
            "src".into(),
            "v".into(),
            SetOptions::default(),
        ));
        assert_eq!(s.execute(Command::Rename("src".into(), "dst".into())), ok());
        assert_eq!(s.execute(Command::Get("src".into())), nil());
        assert_eq!(s.execute(Command::Get("dst".into())), bulk("v"));
        // Rename missing key is an error
        let res = s.execute(Command::Rename("ghost".into(), "dst".into()));
        assert!(matches!(res, Value::Error(_)));
    }

    #[test]
    fn keys_dbsize_and_flushdb() {
        let s = store();
        assert_eq!(s.execute(Command::DbSize), int(0));
        s.execute(Command::MSet(vec![
            ("a".into(), "1".into()),
            ("b".into(), "2".into()),
        ]));
        assert_eq!(s.execute(Command::DbSize), int(2));
        assert_eq!(s.execute(Command::FlushDb), ok());
        assert_eq!(s.execute(Command::DbSize), int(0));
    }

    #[test]
    fn keys_keys_pattern() {
        let s = store();
        s.execute(Command::MSet(vec![
            ("user:1".into(), "a".into()),
            ("user:2".into(), "b".into()),
            ("post:1".into(), "c".into()),
        ]));
        let res = s.execute(Command::Keys("user:*".into()));
        // Returns sorted array of matching keys
        assert_eq!(res, arr(&["user:1", "user:2"]));
        // Wildcard matches all
        let all = s.execute(Command::Keys("*".into()));
        assert_eq!(all, arr(&["post:1", "user:1", "user:2"]));
    }

    #[test]
    fn keys_scan_cursor_zero_returns_all() {
        let s = store();
        s.execute(Command::MSet(vec![
            ("x".into(), "1".into()),
            ("y".into(), "2".into()),
        ]));
        let res = s.execute(Command::Scan(0, None, None));
        // SCAN returns [cursor_bulk, [keys]]
        match res {
            Value::Array(Some(parts)) => {
                assert_eq!(parts[0], Value::BulkString(Some(b"0".to_vec())));
                assert!(matches!(&parts[1], Value::Array(Some(keys)) if keys.len() == 2));
            }
            _ => panic!("expected array"),
        }
    }

    #[test]
    fn keys_scan_nonzero_cursor_returns_empty() {
        let s = store();
        s.execute(Command::Set("k".into(), "v".into(), SetOptions::default()));
        let res = s.execute(Command::Scan(42, None, None));
        match res {
            Value::Array(Some(parts)) => {
                assert_eq!(parts[0], Value::BulkString(Some(b"0".to_vec())));
                assert_eq!(parts[1], Value::Array(Some(vec![])));
            }
            _ => panic!("expected array"),
        }
    }

    #[test]
    fn scan_paginates_with_count() {
        let s = store();
        for i in 0..5 {
            s.execute(Command::Set(
                format!("k{i}"),
                "v".into(),
                SetOptions::default(),
            ));
        }
        // Walk the cursor in pages of 2, collecting every key exactly once.
        let mut seen: Vec<String> = Vec::new();
        let mut cursor = 0u64;
        let mut iterations = 0;
        loop {
            let res = s.execute(Command::Scan(cursor, None, Some(2)));
            let Value::Array(Some(parts)) = res else {
                panic!("expected array")
            };
            let Value::BulkString(Some(c)) = &parts[0] else {
                panic!("expected cursor bulk")
            };
            let next: u64 = String::from_utf8_lossy(c).parse().unwrap();
            let Value::Array(Some(keys)) = &parts[1] else {
                panic!("expected keys array")
            };
            assert!(keys.len() <= 2, "page must honour COUNT");
            for k in keys {
                if let Value::BulkString(Some(d)) = k {
                    seen.push(String::from_utf8_lossy(d).into_owned());
                }
            }
            cursor = next;
            iterations += 1;
            if cursor == 0 {
                break;
            }
            assert!(iterations < 10, "cursor should terminate");
        }
        seen.sort();
        assert_eq!(seen, vec!["k0", "k1", "k2", "k3", "k4"]);
    }

    // ── Expiry ────────────────────────────────────────────────────────────────

    #[test]
    fn expiry_ttl_on_no_ttl_key() {
        let s = store();
        s.execute(Command::Set("k".into(), "v".into(), SetOptions::default()));
        assert_eq!(s.execute(Command::Ttl("k".into())), int(-1));
        assert_eq!(s.execute(Command::PTtl("k".into())), int(-1));
    }

    #[test]
    fn expiry_ttl_missing_key() {
        let s = store();
        assert_eq!(s.execute(Command::Ttl("ghost".into())), int(-2));
        assert_eq!(s.execute(Command::PTtl("ghost".into())), int(-2));
    }

    #[test]
    fn expiry_ttl_after_expire() {
        let s = store();
        s.execute(Command::Set("k".into(), "v".into(), SetOptions::default()));
        s.execute(Command::Expire("k".into(), 100));
        let ttl = s.execute(Command::Ttl("k".into()));
        assert!(matches!(ttl, Value::Integer(n) if n > 0 && n <= 100));
        let pttl = s.execute(Command::PTtl("k".into()));
        assert!(matches!(pttl, Value::Integer(n) if n > 0 && n <= 100_000));
    }

    #[test]
    fn expiry_persist_removes_ttl() {
        let s = store();
        s.execute(Command::Set("k".into(), "v".into(), SetOptions::default()));
        s.execute(Command::Expire("k".into(), 60));
        assert_eq!(s.execute(Command::Persist("k".into())), int(1));
        assert_eq!(s.execute(Command::Ttl("k".into())), int(-1));
        // PERSIST on key with no TTL returns 0
        assert_eq!(s.execute(Command::Persist("k".into())), int(0));
    }

    #[test]
    fn expiry_pexpire_zero_ms_immediate() {
        let s = store();
        s.execute(Command::Set("k".into(), "v".into(), SetOptions::default()));
        s.execute(Command::PExpire("k".into(), 0));
        // Lazy expiry — next access sees it gone
        assert_eq!(s.execute(Command::Get("k".into())), nil());
        assert_eq!(s.execute(Command::Exists(vec!["k".into()])), int(0));
    }

    // ── Hash (additional) ─────────────────────────────────────────────────────

    #[test]
    fn hash_hkeys_hvals() {
        let s = store();
        s.execute(Command::HSet(
            "h".into(),
            vec![("b".into(), "2".into()), ("a".into(), "1".into())],
        ));
        assert_eq!(s.execute(Command::HKeys("h".into())), arr(&["a", "b"]));
        assert_eq!(s.execute(Command::HVals("h".into())), arr(&["1", "2"]));
        // Missing key returns empty array
        assert_eq!(
            s.execute(Command::HKeys("ghost".into())),
            Value::Array(Some(vec![]))
        );
    }

    #[test]
    fn hash_hexists() {
        let s = store();
        s.execute(Command::HSet("h".into(), vec![("f".into(), "v".into())]));
        assert_eq!(s.execute(Command::HExists("h".into(), "f".into())), int(1));
        assert_eq!(s.execute(Command::HExists("h".into(), "no".into())), int(0));
        assert_eq!(
            s.execute(Command::HExists("ghost".into(), "f".into())),
            int(0)
        );
    }

    // ── List (additional) ─────────────────────────────────────────────────────

    #[test]
    fn list_lpushx_rpushx_no_create() {
        let s = store();
        // LPUSHX/RPUSHX on non-existing key return 0 and don't create
        assert_eq!(
            s.execute(Command::LPushX("l".into(), vec!["x".into()])),
            int(0)
        );
        assert_eq!(
            s.execute(Command::RPushX("l".into(), vec!["x".into()])),
            int(0)
        );
        assert_eq!(s.execute(Command::Exists(vec!["l".into()])), int(0));
        // Once the list exists they work normally
        s.execute(Command::LPush("l".into(), vec!["a".into()]));
        assert_eq!(
            s.execute(Command::LPushX("l".into(), vec!["b".into()])),
            int(2)
        );
        assert_eq!(
            s.execute(Command::RPushX("l".into(), vec!["c".into()])),
            int(3)
        );
    }

    #[test]
    fn list_lpop_rpop_with_count() {
        let s = store();
        s.execute(Command::RPush(
            "l".into(),
            vec!["a".into(), "b".into(), "c".into(), "d".into()],
        ));
        assert_eq!(
            s.execute(Command::LPop("l".into(), Some(2))),
            arr(&["a", "b"])
        );
        assert_eq!(
            s.execute(Command::RPop("l".into(), Some(2))),
            arr(&["d", "c"])
        );
    }

    // ── Set (additional) ──────────────────────────────────────────────────────

    #[test]
    fn set_smismember() {
        let s = store();
        s.execute(Command::SAdd("s".into(), vec!["a".into(), "b".into()]));
        let res = s.execute(Command::SMIsMember(
            "s".into(),
            vec!["a".into(), "c".into(), "b".into()],
        ));
        assert_eq!(res, Value::Array(Some(vec![int(1), int(0), int(1)])));
    }

    #[test]
    fn set_sinterstore_sunionstore_sdiffstore() {
        let s = store();
        s.execute(Command::SAdd(
            "a".into(),
            vec!["1".into(), "2".into(), "3".into()],
        ));
        s.execute(Command::SAdd(
            "b".into(),
            vec!["2".into(), "3".into(), "4".into()],
        ));

        assert_eq!(
            s.execute(Command::SInterStore(
                "dst_i".into(),
                vec!["a".into(), "b".into()]
            )),
            int(2)
        );
        assert_eq!(
            s.execute(Command::SMembers("dst_i".into())),
            arr(&["2", "3"])
        );

        assert_eq!(
            s.execute(Command::SUnionStore(
                "dst_u".into(),
                vec!["a".into(), "b".into()]
            )),
            int(4)
        );
        assert_eq!(
            s.execute(Command::SMembers("dst_u".into())),
            arr(&["1", "2", "3", "4"])
        );

        assert_eq!(
            s.execute(Command::SDiffStore(
                "dst_d".into(),
                vec!["a".into(), "b".into()]
            )),
            int(1)
        );
        assert_eq!(s.execute(Command::SMembers("dst_d".into())), arr(&["1"]));
    }

    #[test]
    fn set_spop_removes_member() {
        let s = store();
        s.execute(Command::SAdd(
            "s".into(),
            vec!["a".into(), "b".into(), "c".into()],
        ));
        let popped = s.execute(Command::SPop("s".into(), None));
        // Result must be one of the members
        assert!(matches!(&popped, Value::BulkString(Some(v)) if matches!(
            String::from_utf8_lossy(v).as_ref(), "a" | "b" | "c"
        )));
        // Card decremented
        assert_eq!(s.execute(Command::SCard("s".into())), int(2));
    }

    #[test]
    fn set_spop_with_count() {
        let s = store();
        s.execute(Command::SAdd(
            "s".into(),
            vec!["a".into(), "b".into(), "c".into()],
        ));
        let res = s.execute(Command::SPop("s".into(), Some(2)));
        assert!(matches!(&res, Value::Array(Some(v)) if v.len() == 2));
        assert_eq!(s.execute(Command::SCard("s".into())), int(1));
    }

    #[test]
    fn set_srandmember_no_count() {
        let s = store();
        s.execute(Command::SAdd("s".into(), vec!["x".into(), "y".into()]));
        let res = s.execute(Command::SRandMember("s".into(), None));
        assert!(matches!(&res, Value::BulkString(Some(v))
            if matches!(String::from_utf8_lossy(v).as_ref(), "x" | "y")));
        // SCard unchanged
        assert_eq!(s.execute(Command::SCard("s".into())), int(2));
    }

    #[test]
    fn set_srandmember_with_count() {
        let s = store();
        s.execute(Command::SAdd(
            "s".into(),
            vec!["a".into(), "b".into(), "c".into()],
        ));
        let res = s.execute(Command::SRandMember("s".into(), Some(2)));
        assert!(matches!(&res, Value::Array(Some(v)) if v.len() == 2));
        // Negative count allows duplicates — count by absolute value
        let res_neg = s.execute(Command::SRandMember("s".into(), Some(-5)));
        assert!(matches!(&res_neg, Value::Array(Some(v)) if v.len() == 5));
    }

    // ── Sorted Set (additional) ───────────────────────────────────────────────

    #[test]
    fn zset_zmscore() {
        let s = store();
        s.execute(Command::ZAdd(
            "z".into(),
            ZAddOptions::default(),
            vec![(1.0, "a".into()), (2.5, "b".into())],
        ));
        let res = s.execute(Command::ZMScore(
            "z".into(),
            vec!["a".into(), "ghost".into(), "b".into()],
        ));
        assert_eq!(res, Value::Array(Some(vec![bulk("1"), nil(), bulk("2.5")])));
    }

    #[test]
    fn zset_zrevrangebyscore() {
        let s = store();
        s.execute(Command::ZAdd(
            "z".into(),
            ZAddOptions::default(),
            vec![(1.0, "a".into()), (2.0, "b".into()), (3.0, "c".into())],
        ));
        // ZREVRANGEBYSCORE max min — returns high to low
        assert_eq!(
            s.execute(Command::ZRevRangeByScore(
                "z".into(),
                "3".into(),
                "1".into(),
                false,
                None
            )),
            arr(&["c", "b", "a"])
        );
        // Exclusive bound
        assert_eq!(
            s.execute(Command::ZRevRangeByScore(
                "z".into(),
                "(3".into(),
                "1".into(),
                false,
                None
            )),
            arr(&["b", "a"])
        );
    }

    #[test]
    fn zset_zrevrangebyscore_with_limit() {
        let s = store();
        s.execute(Command::ZAdd(
            "z".into(),
            ZAddOptions::default(),
            vec![
                (1.0, "a".into()),
                (2.0, "b".into()),
                (3.0, "c".into()),
                (4.0, "d".into()),
            ],
        ));
        let res = s.execute(Command::ZRevRangeByScore(
            "z".into(),
            "+inf".into(),
            "-inf".into(),
            false,
            Some((0, 2)),
        ));
        assert_eq!(res, arr(&["d", "c"]));
    }

    #[test]
    fn zset_zrevrange_withscores() {
        let s = store();
        s.execute(Command::ZAdd(
            "z".into(),
            ZAddOptions::default(),
            vec![(1.0, "a".into()), (2.0, "b".into())],
        ));
        let res = s.execute(Command::ZRevRange("z".into(), 0, -1, true));
        assert_eq!(res, arr(&["b", "2", "a", "1"]));
    }

    // ── Snapshot / Restore ────────────────────────────────────────────────────

    #[test]
    fn snapshot_round_trip_all_types() {
        let s = store();
        s.execute(Command::Set(
            "str".into(),
            "hello".into(),
            SetOptions::default(),
        ));
        s.execute(Command::HSet("hash".into(), vec![("f".into(), "v".into())]));
        s.execute(Command::LPush("list".into(), vec!["a".into(), "b".into()]));
        s.execute(Command::SAdd("set".into(), vec!["x".into()]));
        s.execute(Command::ZAdd(
            "zset".into(),
            ZAddOptions::default(),
            vec![(1.5, "m".into())],
        ));

        let entries = s.snapshot();
        assert_eq!(entries.len(), 5);

        let s2 = store();
        s2.restore(entries);

        assert_eq!(s2.execute(Command::Get("str".into())), bulk("hello"));
        assert_eq!(
            s2.execute(Command::HGet("hash".into(), "f".into())),
            bulk("v")
        );
        assert_eq!(
            s2.execute(Command::LRange("list".into(), 0, -1)),
            arr(&["b", "a"])
        );
        assert_eq!(
            s2.execute(Command::SIsMember("set".into(), "x".into())),
            int(1)
        );
        assert_eq!(
            s2.execute(Command::ZScore("zset".into(), "m".into())),
            bulk("1.5")
        );
    }

    #[test]
    fn snapshot_skips_expired_keys() {
        use std::time::Duration;
        let s = store();
        s.execute(Command::Set(
            "live".into(),
            "v".into(),
            SetOptions::default(),
        ));
        s.execute(Command::PSetEx("dead".into(), 1, "v".into()));
        std::thread::sleep(Duration::from_millis(10));

        let entries = s.snapshot();
        assert!(entries.iter().any(|e| e.key == "live"));
        assert!(!entries.iter().any(|e| e.key == "dead"));
    }

    #[test]
    fn restore_skips_already_expired() {
        let s = store();
        let entry = SnapshotEntry {
            key: "ghost".into(),
            value: SnapshotValue::Str("v".into()),
            expires_at_ms: Some(1),
        };
        s.restore(vec![entry]);
        assert_eq!(s.execute(Command::DbSize), int(0));
    }

    #[test]
    fn snapshot_preserves_ttl() {
        let s = store();
        s.execute(Command::SetEx("k".into(), 60, "v".into()));

        let entries = s.snapshot();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].expires_at_ms.is_some());

        let s2 = store();
        s2.restore(entries);

        let ttl = s2.execute(Command::Ttl("k".into()));
        assert!(matches!(ttl, Value::Integer(n) if n > 0 && n <= 60));
    }

    // ── Rate limiting (RLSET / RLCHECK) ───────────────────────────────────────

    /// Unpack an RLCHECK reply into (allowed, remaining, retry_after_ms).
    fn rl(v: Value) -> (i64, i64, i64) {
        match v {
            Value::Array(Some(items)) => match items.as_slice() {
                [Value::Integer(a), Value::Integer(r), Value::Integer(t)] => (*a, *r, *t),
                other => panic!("unexpected RLCHECK reply shape: {:?}", other),
            },
            other => panic!("unexpected RLCHECK reply: {:?}", other),
        }
    }

    #[test]
    fn rlset_and_check_enforce_limit() {
        let s = store();
        assert_eq!(s.execute(Command::RlSet("api".into(), 3, 60)), ok());
        for expected_remaining in [2, 1, 0] {
            let (allowed, remaining, retry) = rl(s.execute(Command::RlCheck("api".into(), None)));
            assert_eq!(allowed, 1);
            assert_eq!(remaining, expected_remaining);
            assert_eq!(retry, 0);
        }
        let (allowed, remaining, retry) = rl(s.execute(Command::RlCheck("api".into(), None)));
        assert_eq!(allowed, 0);
        assert_eq!(remaining, 0);
        assert!(retry > 0 && retry <= 60_000, "retry_after_ms = {}", retry);
    }

    #[test]
    fn rlcheck_inline_config_creates_limiter() {
        let s = store();
        let key = "ip:10.0.0.1".to_string();
        let (allowed, _, _) = rl(s.execute(Command::RlCheck(key.clone(), Some((2, 60)))));
        assert_eq!(allowed, 1);
        let (allowed, _, _) = rl(s.execute(Command::RlCheck(key.clone(), Some((2, 60)))));
        assert_eq!(allowed, 1);
        let (allowed, _, _) = rl(s.execute(Command::RlCheck(key, Some((2, 60)))));
        assert_eq!(allowed, 0);
    }

    #[test]
    fn rlcheck_unconfigured_errors() {
        let s = store();
        let r = s.execute(Command::RlCheck("nope".into(), None));
        assert!(matches!(&r, Value::Error(e) if e.contains("no rate limit configured")));
    }

    #[test]
    fn rl_wrongtype_interactions() {
        let s = store();
        s.execute(Command::Set(
            "str".into(),
            "v".into(),
            SetOptions::default(),
        ));
        let r = s.execute(Command::RlCheck("str".into(), Some((5, 60))));
        assert!(matches!(&r, Value::Error(e) if e.contains("WRONGTYPE")));
        let r = s.execute(Command::RlSet("str".into(), 5, 60));
        assert!(matches!(&r, Value::Error(e) if e.contains("WRONGTYPE")));

        s.execute(Command::RlSet("rl".into(), 5, 60));
        let r = s.execute(Command::Get("rl".into()));
        assert!(matches!(&r, Value::Error(e) if e.contains("WRONGTYPE")));
        assert_eq!(
            s.execute(Command::Type("rl".into())),
            Value::SimpleString("ratelimit".into())
        );
    }

    #[test]
    fn rlset_reconfig_keeps_recorded_attempts() {
        let s = store();
        s.execute(Command::RlSet("api".into(), 2, 60));
        rl(s.execute(Command::RlCheck("api".into(), None)));
        // Raise the limit: the one recorded attempt still counts against it.
        s.execute(Command::RlSet("api".into(), 3, 60));
        let (allowed, remaining, _) = rl(s.execute(Command::RlCheck("api".into(), None)));
        assert_eq!((allowed, remaining), (1, 1));
        let (allowed, _, _) = rl(s.execute(Command::RlCheck("api".into(), None)));
        assert_eq!(allowed, 1);
        let (allowed, _, _) = rl(s.execute(Command::RlCheck("api".into(), None)));
        assert_eq!(allowed, 0);
    }

    #[test]
    fn rl_snapshot_roundtrip_preserves_state() {
        let s = store();
        s.execute(Command::RlSet("api".into(), 2, 60));
        rl(s.execute(Command::RlCheck("api".into(), None)));
        rl(s.execute(Command::RlCheck("api".into(), None)));

        let s2 = store();
        s2.restore(s.snapshot());
        let (allowed, remaining, retry) = rl(s2.execute(Command::RlCheck("api".into(), None)));
        assert_eq!((allowed, remaining), (0, 0));
        assert!(retry > 0);
    }

    // ── JSON (JSET / JGET / JMERGE) ───────────────────────────────────────────

    fn jset(s: &KeyValueStore, key: &str, path: &str, value: &str) -> Value {
        s.execute(Command::JSet(key.into(), path.into(), value.into()))
    }
    fn jget(s: &KeyValueStore, key: &str, path: Option<&str>) -> Value {
        s.execute(Command::JGet(key.into(), path.map(String::from)))
    }

    #[test]
    fn jset_jget_root_and_paths() {
        let s = store();
        assert_eq!(
            jset(&s, "doc", "$", r#"{"user":{"name":"amy"},"items":[1,2,3]}"#),
            ok()
        );
        // serde_json serializes object keys in sorted order — deterministic
        // output regardless of insertion order.
        assert_eq!(
            jget(&s, "doc", None),
            bulk(r#"{"items":[1,2,3],"user":{"name":"amy"}}"#)
        );
        assert_eq!(jget(&s, "doc", Some("$.user.name")), bulk(r#""amy""#));
        assert_eq!(jget(&s, "doc", Some("$.items[1]")), bulk("2"));
        // Set a nested field and an array element in place.
        assert_eq!(jset(&s, "doc", "$.user.age", "30"), ok());
        assert_eq!(jget(&s, "doc", Some("$.user.age")), bulk("30"));
        assert_eq!(jset(&s, "doc", "$.items[0]", "9"), ok());
        assert_eq!(jget(&s, "doc", Some("$.items")), bulk("[9,2,3]"));
        // Missing path → nil; missing key → nil.
        assert_eq!(jget(&s, "doc", Some("$.nope.deep")), nil());
        assert_eq!(jget(&s, "ghost", None), nil());
        // TYPE reports json.
        assert_eq!(
            s.execute(Command::Type("doc".into())),
            Value::SimpleString("json".into())
        );
    }

    #[test]
    fn jset_autocreates_intermediate_objects() {
        let s = store();
        // Fresh key, deep field path: intermediate objects are created.
        assert_eq!(jset(&s, "doc", "$.a.b.c", "1"), ok());
        assert_eq!(jget(&s, "doc", None), bulk(r#"{"a":{"b":{"c":1}}}"#));
        // Fresh key with a leading index cannot apply — and must not create
        // the key as a side effect.
        let r = jset(&s, "fresh", "$[0]", "1");
        assert!(matches!(&r, Value::Error(_)));
        assert_eq!(s.execute(Command::Exists(vec!["fresh".into()])), int(0));
        // Index out of bounds errors.
        let r = jset(&s, "doc", "$.a.b.c[5]", "1");
        assert!(matches!(&r, Value::Error(e) if e.contains("not an array")));
    }

    #[test]
    fn jmerge_rfc7386_semantics() {
        let s = store();
        jset(
            &s,
            "doc",
            "$",
            r#"{"title":"old","meta":{"draft":true,"v":1}}"#,
        );
        // Deep merge + null removal.
        assert_eq!(
            s.execute(Command::JMerge(
                "doc".into(),
                r#"{"title":"new","meta":{"draft":null}}"#.into()
            )),
            ok()
        );
        assert_eq!(
            jget(&s, "doc", None),
            bulk(r#"{"meta":{"v":1},"title":"new"}"#)
        );
        // Merge into a missing key creates the document.
        assert_eq!(
            s.execute(Command::JMerge("fresh".into(), r#"{"a":1}"#.into())),
            ok()
        );
        assert_eq!(jget(&s, "fresh", None), bulk(r#"{"a":1}"#));
        // A null patch deletes the key.
        assert_eq!(
            s.execute(Command::JMerge("fresh".into(), "null".into())),
            ok()
        );
        assert_eq!(jget(&s, "fresh", None), nil());
    }

    #[test]
    fn json_wrongtype_and_invalid_input() {
        let s = store();
        s.execute(Command::Set(
            "str".into(),
            "v".into(),
            SetOptions::default(),
        ));
        assert!(matches!(&jset(&s, "str", "$", "1"), Value::Error(e) if e.contains("WRONGTYPE")));
        assert!(matches!(&jget(&s, "str", None), Value::Error(e) if e.contains("WRONGTYPE")));
        jset(&s, "doc", "$", "{}");
        let r = s.execute(Command::Get("doc".into()));
        assert!(matches!(&r, Value::Error(e) if e.contains("WRONGTYPE")));
        // Invalid JSON / invalid path.
        assert!(
            matches!(&jset(&s, "doc", "$", "{oops"), Value::Error(e) if e.contains("invalid JSON"))
        );
        assert!(
            matches!(&jset(&s, "doc", "$.a[x]", "1"), Value::Error(e) if e.contains("bad index"))
        );
    }

    #[test]
    fn json_snapshot_roundtrip() {
        let s = store();
        jset(&s, "doc", "$", r#"{"n":42,"arr":[true,null,"x"]}"#);
        let s2 = store();
        s2.restore(s.snapshot());
        assert_eq!(
            s2.execute(Command::JGet("doc".into(), None)),
            bulk(r#"{"arr":[true,null,"x"],"n":42}"#)
        );
        assert_eq!(
            s2.execute(Command::Type("doc".into())),
            Value::SimpleString("json".into())
        );
    }

    // ── Live-query initial state ──────────────────────────────────────────────

    #[test]
    fn matching_key_values_globs_caps_and_skips_expired() {
        use std::time::Duration;
        let s = store();
        s.execute(Command::Set(
            "cart:1".into(),
            "a".into(),
            SetOptions::default(),
        ));
        s.execute(Command::Set(
            "cart:2".into(),
            "b".into(),
            SetOptions::default(),
        ));
        s.execute(Command::Set(
            "other:1".into(),
            "c".into(),
            SetOptions::default(),
        ));
        s.execute(Command::LPush("cart:list".into(), vec!["x".into()]));
        s.execute(Command::PSetEx("cart:dead".into(), 1, "d".into()));
        std::thread::sleep(Duration::from_millis(10));

        let mut kvs = s.matching_key_values("cart:*", 100);
        kvs.sort_by(|(a, _), (b, _)| a.cmp(b));
        assert_eq!(kvs.len(), 3, "expired key must be skipped: {kvs:?}");
        assert_eq!(kvs[0], ("cart:1".to_string(), bulk("a")));
        assert_eq!(kvs[1], ("cart:2".to_string(), bulk("b")));
        // Collection types come back as type-name markers, like get_current.
        assert_eq!(
            kvs[2],
            (
                "cart:list".to_string(),
                Value::SimpleString("list".to_string())
            )
        );
        // Cap respected.
        assert_eq!(s.matching_key_values("cart:*", 2).len(), 2);
        // Non-matching pattern.
        assert!(s.matching_key_values("nope:*", 100).is_empty());
    }

    #[test]
    fn rl_window_slides_and_inline_limiter_expires() {
        use std::time::Duration;
        let s = store();
        // Persistent limiter: 1 attempt per 1-second window.
        s.execute(Command::RlSet("persist".into(), 1, 1));
        let (allowed, _, _) = rl(s.execute(Command::RlCheck("persist".into(), None)));
        assert_eq!(allowed, 1);
        let (allowed, _, retry) = rl(s.execute(Command::RlCheck("persist".into(), None)));
        assert_eq!(allowed, 0);
        assert!(retry > 0 && retry <= 1_000);
        // Auto-created limiter with a 1-second window.
        let (allowed, _, _) = rl(s.execute(Command::RlCheck("perip".into(), Some((1, 1)))));
        assert_eq!(allowed, 1);

        std::thread::sleep(Duration::from_millis(1_050));

        // The window slid past the recorded attempt: allowed again.
        let (allowed, _, _) = rl(s.execute(Command::RlCheck("persist".into(), None)));
        assert_eq!(allowed, 1);
        // The auto-created limiter expired with its window — bare RLCHECK
        // finds no config.
        let r = s.execute(Command::RlCheck("perip".into(), None));
        assert!(matches!(&r, Value::Error(e) if e.contains("no rate limit configured")));
    }
}
