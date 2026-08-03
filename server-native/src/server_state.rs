//! The shared server state every connection writes through: AOF handle,
//! replica registry, replica/primary role, and write de-duplication.

use crate::*;

pub(crate) struct ServerState {
    pub(crate) snap: Arc<SnapshotConfig>,
    pub(crate) aof: Option<Arc<AofWriter>>,
    pub(crate) replicas: ReplRegistry,
    /// true = currently acting as a read-only replica
    pub(crate) is_replica: std::sync::atomic::AtomicBool,
    /// Exactly-once bookkeeping for DEDUP-wrapped writes: client id →
    /// (highest id applied, last-seen ms). Clients send monotonically
    /// increasing ids and replay in order, so a single high-water mark per
    /// client suffices — no seen-set. In-memory only: a server restart
    /// reopens the (already narrow) duplicate window, which is documented.
    pub(crate) dedup: std::sync::Mutex<HashMap<String, (u64, u64)>>,
    /// Ephemeral (`ESET`) keys → the connection that currently owns them.
    ///
    /// Ownership transfers on each `ESET`, which is what makes multiple tabs
    /// work: two tabs both setting `presence:user:42` leave the *later* one as
    /// owner, so the first tab closing does not mark the user offline. Only the
    /// owning connection's close deletes the key.
    pub(crate) ephemeral: std::sync::Mutex<HashMap<String, u64>>,
    /// Set when a dedup high-water mark advances; cleared once persisted.
    pub(crate) dedup_dirty: std::sync::atomic::AtomicBool,
}

impl ServerState {
    /// Record `conn_id` as the owner of an ephemeral key, replacing any
    /// previous owner.
    pub(crate) fn claim_ephemeral(&self, key: &str, conn_id: u64) {
        if let Ok(mut map) = self.ephemeral.lock() {
            map.insert(key.to_string(), conn_id);
        }
    }

    /// Keys still owned by `conn_id`, removed from the registry. Called once
    /// when a connection closes.
    pub(crate) fn take_ephemeral_for(&self, conn_id: u64) -> Vec<String> {
        let Ok(mut map) = self.ephemeral.lock() else {
            return Vec::new();
        };
        let owned: Vec<String> = map
            .iter()
            .filter(|(_, id)| **id == conn_id)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &owned {
            map.remove(k);
        }
        owned
    }
}

/// Sweep dedup client entries idle longer than this once the map is large.
pub(crate) const DEDUP_IDLE_MS: u64 = 24 * 60 * 60 * 1000;

pub(crate) const DEDUP_SWEEP_THRESHOLD: usize = 10_000;

impl ServerState {
    pub(crate) fn is_replica(&self) -> bool {
        self.is_replica.load(Ordering::Relaxed)
    }

    pub(crate) fn promote_to_primary(&self) {
        self.is_replica.store(false, Ordering::Relaxed);
        info!("REPLICAOF NO ONE: promoted to primary — writes now accepted");
    }

    /// True when a write must be RESP-encoded for the durability/replication
    /// path even if no other consumer needs it.
    pub(crate) fn needs_write_log(&self) -> bool {
        self.aof.is_some() || !self.replicas.is_empty()
    }

    /// Record a DEDUP-wrapped write. Returns `true` when `id` was already
    /// applied for this client (the write must be skipped). Marks the id
    /// *before* execution so a crash between check and execute can never
    /// double-apply.
    pub(crate) fn dedup_seen(&self, client: &str, id: u64) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut map = self.dedup.lock().expect("dedup mutex poisoned");
        if map.len() > DEDUP_SWEEP_THRESHOLD {
            map.retain(|_, (_, seen)| now.saturating_sub(*seen) < DEDUP_IDLE_MS);
        }
        match map.get_mut(client) {
            Some((hwm, seen)) => {
                *seen = now;
                if id <= *hwm {
                    true
                } else {
                    *hwm = id;
                    self.dedup_dirty.store(true, Ordering::Relaxed);
                    false
                }
            }
            None => {
                map.insert(client.to_string(), (id, now));
                self.dedup_dirty.store(true, Ordering::Relaxed);
                false
            }
        }
    }

    /// Called after every successful write: appends to AOF and fans out to replicas.
    pub(crate) async fn on_write(&self, resp: &[u8]) {
        if let Some(aof) = &self.aof {
            aof.append(resp).await;
        }
        if self.replicas.is_empty() {
            return;
        }
        self.replicas.fan_out(resp.to_vec()).await;
    }

    /// Path of the dedup sidecar, alongside the snapshot.
    pub(crate) fn dedup_path(&self) -> std::path::PathBuf {
        self.snap.path.with_extension("dedup")
    }

    /// Persist dedup high-water marks so exactly-once delivery survives a
    /// restart. Written atomically (temp + rename) and only when a mark has
    /// advanced. The map is one `u64` per client, so this stays small enough to
    /// flush far more often than the snapshot.
    pub(crate) async fn persist_dedup(&self) {
        if !self.dedup_dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let marks: Vec<(String, u64)> = match self.dedup.lock() {
            Ok(map) => map.iter().map(|(c, (hwm, _))| (c.clone(), *hwm)).collect(),
            Err(_) => return,
        };
        let path = self.dedup_path();
        let tmp = temp_sibling(&path, "dedup");
        match rmp_serde::to_vec(&marks) {
            Err(e) => warn!("Dedup serialize failed: {}", e),
            Ok(bytes) => match write_private(&tmp, &bytes).await {
                Err(e) => warn!("Dedup write failed: {}", e),
                Ok(()) => match tokio::fs::rename(&tmp, &path).await {
                    Err(e) => warn!("Dedup rename failed: {}", e),
                    Ok(()) => sync_parent_dir(&path).await,
                },
            },
        }
    }

    /// Restore dedup marks at boot. `seen` timestamps are not persisted — they
    /// only drive idle sweeping, so restored entries start their idle clock now.
    pub(crate) async fn load_dedup(&self) {
        let path = self.dedup_path();
        let Ok(bytes) = tokio::fs::read(&path).await else {
            return;
        };
        match rmp_serde::from_slice::<Vec<(String, u64)>>(&bytes) {
            Err(e) => warn!("Dedup sidecar unreadable ({}), ignoring: {:?}", e, path),
            Ok(marks) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if let Ok(mut map) = self.dedup.lock() {
                    let count = marks.len();
                    for (client, hwm) in marks {
                        map.insert(client, (hwm, now));
                    }
                    info!("Restored {} dedup high-water mark(s)", count);
                }
            }
        }
    }

    /// Save snapshot, reset the dirty counter, then truncate AOF (snapshot subsumes the log).
    pub(crate) async fn save(&self, store: &KeyValueStore) {
        self.persist_dedup().await;
        save_snapshot(store, &self.snap).await;
        store.reset_dirty();
        if let Some(aof) = &self.aof {
            aof.truncate().await;
        }
    }
}
