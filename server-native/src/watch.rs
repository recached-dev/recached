//! Watched keys: the registry behind WATCH's compare-and-set transactions and
//! the live-query (QSUB) subscriptions that share it.

use crate::*;

pub(crate) type WatchNotif = (String, Value);

pub(crate) type WatchMap = HashMap<String, Vec<(u64, mpsc::UnboundedSender<WatchNotif>)>>;

/// Watched-key and live-query registry. `watched_keys` / `watched_patterns`
/// mirror the map lengths (updated by every writer while holding the lock) so
/// the per-write hot path can skip the mutexes entirely when nothing is
/// watched.
pub(crate) struct WatchHub {
    /// Exact-key watchers (WATCH).
    pub(crate) map: tokio::sync::Mutex<WatchMap>,
    pub(crate) watched_keys: AtomicUsize,
    /// Glob-pattern subscribers (QSUB live queries), keyed by pattern.
    pub(crate) patterns: tokio::sync::Mutex<WatchMap>,
    pub(crate) watched_patterns: AtomicUsize,
}

impl WatchHub {
    pub(crate) fn new() -> WatchRegistry {
        Arc::new(WatchHub {
            map: tokio::sync::Mutex::new(HashMap::new()),
            watched_keys: AtomicUsize::new(0),
            patterns: tokio::sync::Mutex::new(HashMap::new()),
            watched_patterns: AtomicUsize::new(0),
        })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.watched_keys.load(Ordering::Relaxed) == 0
            && self.watched_patterns.load(Ordering::Relaxed) == 0
    }

    /// Call after mutating the key map, while still holding the lock.
    pub(crate) fn sync_len(&self, map: &WatchMap) {
        self.watched_keys.store(map.len(), Ordering::Relaxed);
    }

    /// Call after mutating the pattern map, while still holding the lock.
    pub(crate) fn sync_patterns_len(&self, map: &WatchMap) {
        self.watched_patterns.store(map.len(), Ordering::Relaxed);
    }
}

pub(crate) type WatchRegistry = Arc<WatchHub>;

/// Drop all of `conn_id`'s live-query subscriptions. Called on QUNSUB (all
/// form) and on connection close.
pub(crate) async fn unregister_all_qsubs(
    registry: &WatchRegistry,
    conn_id: u64,
    qsub_patterns: &mut HashSet<String>,
) {
    if qsub_patterns.is_empty() {
        return;
    }
    let mut pats = registry.patterns.lock().await;
    for p in qsub_patterns.drain() {
        if let Some(subs) = pats.get_mut(&p) {
            subs.retain(|(id, _)| *id != conn_id);
            if subs.is_empty() {
                pats.remove(&p);
            }
        }
    }
    registry.sync_patterns_len(&pats);
}

/// Drop all of `conn_id`'s WATCH registrations and clear `watched_keys`.
/// Called at every transaction boundary (EXEC, DISCARD) and on connection close,
/// matching Redis semantics that WATCH state is flushed by EXEC/DISCARD.
pub(crate) async fn unregister_all_watches(
    registry: &WatchRegistry,
    conn_id: u64,
    watched_keys: &mut HashSet<String>,
) {
    if watched_keys.is_empty() {
        return;
    }
    let mut reg = registry.map.lock().await;
    for key in watched_keys.drain() {
        if let Some(subs) = reg.get_mut(&key) {
            subs.retain(|(id, _)| *id != conn_id);
            if subs.is_empty() {
                reg.remove(&key);
            }
        }
    }
    registry.sync_len(&reg);
}

// ── helpers ──────────────────────────────────────────────────────────────────
