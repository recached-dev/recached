//! Durability: RDB-style snapshots and the append-only file, plus the
//! private-permission file helpers both rely on.

use crate::*;

/// Write `bytes` to `path`, creating it readable only by this user.
///
/// Snapshots, the AOF, and the dedup sidecar are plaintext MessagePack dumps of
/// the keyspace. `fs::write` creates with the process umask — `0644` on a
/// typical host — so any local user could read the entire cache. The
/// documentation told operators to protect these files with filesystem
/// permissions; the server should never have been relying on that.
///
/// Permissions are also set explicitly after opening, so a file left behind
/// `0644` by an earlier version is tightened on the next write rather than
/// keeping its old mode forever.
/// Writes are fsynced before returning. Every caller is writing state that has
/// to survive a crash — a snapshot about to be renamed into place, or the dedup
/// high-water marks that stop a replayed write being applied twice — so the
/// barrier belongs here rather than at each call site.
pub(crate) async fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut f = opts.open(path).await?;
    #[cfg(unix)]
    restrict_permissions(&f).await;
    f.write_all(bytes).await?;
    f.flush().await?;
    // `sync_all`, not `sync_data`: this file was just created, so its metadata
    // is part of what has to reach the device.
    f.sync_all().await?;
    Ok(())
}

/// fsync the directory holding `path`, making a `rename` into it durable.
///
/// Renaming a fsynced temp file over the target is atomic with respect to
/// readers, but the *directory entry* is itself just a write: without this, a
/// crash can leave the old file, or no file, despite the new contents being
/// safely on disk. Only meaningful on unix — Windows has no directory handle to
/// sync — so the call is compiled out elsewhere.
#[cfg(unix)]
pub(crate) async fn sync_parent_dir(path: &std::path::Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    // An empty parent means the path was relative with no directory component.
    let dir = if dir.as_os_str().is_empty() {
        std::path::Path::new(".")
    } else {
        dir
    };
    match tokio::fs::File::open(dir).await {
        Ok(f) => {
            if let Err(e) = f.sync_all().await {
                warn!("Directory fsync failed for {:?}: {}", dir, e);
            }
        }
        Err(e) => warn!("Could not open {:?} to fsync: {}", dir, e),
    }
}

#[cfg(not(unix))]
pub(crate) async fn sync_parent_dir(_path: &std::path::Path) {}

/// Tighten an already-open file to `0600`, ignoring failure.
///
/// Best-effort by design: on a filesystem that cannot represent unix modes this
/// is not something to fail a write over, and the caller has already created the
/// file with the right mode where the platform allows it.
#[cfg(unix)]
pub(crate) async fn restrict_permissions(f: &tokio::fs::File) {
    use std::os::unix::fs::PermissionsExt;
    let _ = f
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .await;
}

/// Path for a temp file alongside `path`, distinct per process.
///
/// The previous fixed `.tmp` name meant two servers sharing a directory would
/// clobber each other's half-written snapshot, and made the target predictable
/// to anyone who could already write to that directory. Residual: this is not
/// unguessable, so it is a defence against collision rather than against an
/// attacker who already controls the data directory.
pub(crate) fn temp_sibling(path: &std::path::Path, tag: &str) -> PathBuf {
    path.with_extension(format!("{tag}.{}.tmp", std::process::id()))
}

// ── snapshot persistence ──────────────────────────────────────────────────────

pub(crate) struct SnapshotConfig {
    pub(crate) path: PathBuf,
    pub(crate) last_save: AtomicI64,
}

pub(crate) async fn save_snapshot(store: &KeyValueStore, cfg: &SnapshotConfig) {
    let entries = store.snapshot();
    let count = entries.len();
    let tmp = temp_sibling(&cfg.path, "snap");
    match rmp_serde::to_vec(&entries) {
        Err(e) => warn!("Snapshot serialize failed: {}", e),
        Ok(bytes) => match write_private(&tmp, &bytes).await {
            Err(e) => warn!("Snapshot write failed: {}", e),
            Ok(()) => match tokio::fs::rename(&tmp, &cfg.path).await {
                Err(e) => warn!("Snapshot rename failed: {}", e),
                Ok(()) => {
                    sync_parent_dir(&cfg.path).await;
                    cfg.last_save.store(now_unix_secs(), Ordering::Relaxed);
                    info!("Snapshot saved: {} entries → {:?}", count, cfg.path);
                }
            },
        },
    }
}

pub(crate) async fn load_snapshot(store: &KeyValueStore, path: &std::path::Path) -> bool {
    match tokio::fs::read(path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!("No snapshot at {:?}, starting fresh", path);
            false
        }
        Err(e) => {
            warn!("Snapshot read failed: {}", e);
            false
        }
        Ok(bytes) => match rmp_serde::from_slice::<Vec<SnapshotEntry>>(&bytes) {
            Err(e) => {
                warn!("Snapshot deserialize failed: {}", e);
                false
            }
            Ok(entries) => {
                let count = entries.len();
                store.restore(entries);
                info!("Snapshot loaded: {} entries ← {:?}", count, path);
                true
            }
        },
    }
}

// ── AOF ───────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum AofSync {
    Always,
    EverySec,
    No,
}

pub(crate) struct AofWriter {
    #[allow(dead_code)]
    pub(crate) path: PathBuf,
    pub(crate) file: tokio::sync::Mutex<tokio::fs::File>,
    pub(crate) sync: AofSync,
}

impl AofWriter {
    pub(crate) async fn open(path: PathBuf, sync: AofSync) -> std::io::Result<Self> {
        let mut opts = tokio::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let file = opts.open(&path).await?;
        // An AOF written by an earlier version is likely to be 0644 — tighten it
        // on open, since `mode()` only applies to files this call creates.
        #[cfg(unix)]
        restrict_permissions(&file).await;
        Ok(Self {
            path,
            file: tokio::sync::Mutex::new(file),
            sync,
        })
    }

    pub(crate) async fn append(&self, resp: &[u8]) {
        let mut f = self.file.lock().await;
        if f.write_all(resp).await.is_err() {
            warn!("AOF write failed");
            return;
        }
        if self.sync == AofSync::Always {
            // `flush()` alone only pushes tokio's buffer into a `write` syscall,
            // which leaves the bytes in the page cache — surviving a process
            // crash but not a power loss or kernel panic. `always` exists
            // precisely to survive the latter, so it has to reach the device.
            if let Err(e) = f.flush().await {
                warn!("AOF flush failed: {}", e);
                return;
            }
            if let Err(e) = f.sync_data().await {
                warn!("AOF fsync failed: {}", e);
            }
        }
    }

    /// Flush and fsync. Called on the `everysec` ticker and before shutdown.
    ///
    /// `sync_data` rather than `sync_all`: the AOF is append-only, so its
    /// metadata beyond the length carries nothing worth an extra barrier.
    pub(crate) async fn flush(&self) {
        let mut f = self.file.lock().await;
        if let Err(e) = f.flush().await {
            warn!("AOF flush failed: {}", e);
            return;
        }
        if let Err(e) = f.sync_data().await {
            warn!("AOF fsync failed: {}", e);
        }
    }

    pub(crate) async fn truncate(&self) {
        let f = self.file.lock().await;
        match f.set_len(0).await {
            // The truncation itself must be durable, or a crash can resurrect a
            // log the snapshot has already subsumed and replay it on top.
            Ok(()) => match f.sync_all().await {
                Ok(()) => info!("AOF truncated after snapshot save"),
                Err(e) => warn!("AOF truncate fsync failed: {}", e),
            },
            Err(e) => warn!("AOF truncate failed: {}", e),
        }
    }
}

pub(crate) async fn replay_aof(store: &KeyValueStore, path: &std::path::Path) -> usize {
    let bytes = match tokio::fs::read(path).await {
        Err(e) if e.kind() == ErrorKind::NotFound => return 0,
        Err(e) => {
            warn!("AOF read failed: {}", e);
            return 0;
        }
        Ok(b) => b,
    };
    let mut replayed = 0usize;
    let mut offset = 0;
    while offset < bytes.len() {
        match Value::parse(&bytes[offset..]) {
            Ok((value, consumed)) => {
                offset += consumed;
                // Writes are recorded via `on_write` in RESP3 Push form (`>N`);
                // normalise to Array so Command::from_value can parse them.
                let normalised = match value {
                    Value::Push(inner) => Value::Array(Some(inner)),
                    other => other,
                };
                if let Ok(cmd) = Command::from_value(normalised) {
                    store.execute(cmd);
                    replayed += 1;
                }
            }
            Err(e) if e.is_incomplete() => break,
            Err(_) => {
                warn!("AOF corrupted at offset {}, stopping replay", offset);
                break;
            }
        }
    }
    if replayed > 0 {
        info!("AOF replayed: {} commands ← {:?}", replayed, path);
    }
    replayed
}

// ── Replication ───────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────────
// Durability: the guarantees `RECACHED_AOF_SYNC` and snapshot saving advertise.
//
// These assert reachability and effect rather than device-level durability — a
// unit test cannot pull the power. What they pin is that the fsync path is
// actually taken, that it does not corrupt or lose data, and that the pieces a
// crash-consistency argument depends on (temp file synced before rename, parent
// directory synced after) are wired up.
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod durability_tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "recached_dur_{}_{}_{}",
            name,
            std::process::id(),
            next_conn_id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[tokio::test]
    async fn aof_always_survives_a_reopen_with_every_byte_intact() {
        // `always` previously called flush(), which reaches the page cache and
        // not the device. The observable part of the fix is that the fsync path
        // runs and is still byte-exact.
        let dir = scratch("aof_always");
        let path = dir.join("a.aof");
        let w = AofWriter::open(path.clone(), AofSync::Always)
            .await
            .unwrap();
        for i in 0..64 {
            w.append(format!("*1\r\n${}\r\n{}\r\n", i.to_string().len(), i).as_bytes())
                .await;
        }
        drop(w);

        let on_disk = tokio::fs::read(&path).await.unwrap();
        let expected: Vec<u8> = (0..64)
            .flat_map(|i| format!("*1\r\n${}\r\n{}\r\n", i.to_string().len(), i).into_bytes())
            .collect();
        assert_eq!(on_disk, expected, "fsync must not disturb the byte stream");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn aof_everysec_flush_is_idempotent_and_lossless() {
        // The everysec ticker calls flush() on a cadence, including when nothing
        // has been appended since the last tick.
        let dir = scratch("aof_everysec");
        let path = dir.join("b.aof");
        let w = AofWriter::open(path.clone(), AofSync::EverySec)
            .await
            .unwrap();
        w.append(b"*1\r\n$4\r\nPING\r\n").await;
        w.flush().await;
        w.flush().await; // nothing new to sync
        assert_eq!(
            tokio::fs::read(&path).await.unwrap(),
            b"*1\r\n$4\r\nPING\r\n"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn truncating_the_aof_leaves_it_empty_and_reusable() {
        // Truncation follows a snapshot, and is now fsynced so a crash cannot
        // resurrect a log the snapshot already subsumed. It must also leave the
        // handle writable — the server keeps appending to it afterwards.
        let dir = scratch("aof_trunc");
        let path = dir.join("c.aof");
        let w = AofWriter::open(path.clone(), AofSync::No).await.unwrap();
        w.append(b"*1\r\n$4\r\nPING\r\n").await;
        w.truncate().await;
        assert_eq!(tokio::fs::metadata(&path).await.unwrap().len(), 0);

        w.append(b"*1\r\n$4\r\nECHO\r\n").await;
        w.flush().await;
        assert_eq!(
            tokio::fs::read(&path).await.unwrap(),
            b"*1\r\n$4\r\nECHO\r\n",
            "the writer must still be usable after truncation"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn a_snapshot_lands_atomically_and_leaves_no_temp_file() {
        // The temp file is fsynced, renamed over the target, and then the
        // directory is fsynced. A leftover temp file would mean the rename never
        // happened, which is the failure this sequence exists to prevent.
        let dir = scratch("snap");
        let path = dir.join("dump.rdb");
        let store = KeyValueStore::new();
        for i in 0..32 {
            store.execute(Command::Set(
                format!("k{i}"),
                format!("v{i}").into_bytes(),
                Default::default(),
            ));
        }
        let cfg = SnapshotConfig {
            path: path.clone(),
            last_save: AtomicI64::new(0),
        };
        save_snapshot(&store, &cfg).await;

        assert!(path.exists(), "snapshot must exist after save");
        assert!(
            cfg.last_save.load(Ordering::Relaxed) > 0,
            "a completed save must advance LASTSAVE"
        );

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );

        // And the bytes are a snapshot we can actually read back.
        let restored = KeyValueStore::new();
        assert!(load_snapshot(&restored, &path).await);
        assert_eq!(
            restored.execute(Command::Get("k7".into())),
            Value::BulkString(Some(b"v7".to_vec()))
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn syncing_a_parent_directory_tolerates_odd_paths() {
        // Best-effort by contract: a path with no parent, or one that does not
        // exist, must warn rather than panic or hang. The snapshot path is
        // frequently relative ("recached.rdb"), which has an empty parent.
        sync_parent_dir(std::path::Path::new("recached.rdb")).await;
        sync_parent_dir(std::path::Path::new("/")).await;
        sync_parent_dir(std::path::Path::new("/nonexistent-recached-dir/x.rdb")).await;
    }

    #[test]
    fn a_sync_token_cannot_grant_an_over_long_pattern() {
        // Token patterns reach glob_match without passing through the command
        // parser, so the cap has to be repeated there. These are matched once
        // per key per write — the most expensive place a pattern can sit.
        use base64::Engine as _;
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let secret = "s3cret";
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let mint = |payload: &str| {
            let p = engine.encode(payload);
            let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
            mac.update(p.as_bytes());
            format!("{}.{}", p, engine.encode(mac.finalize().into_bytes()))
        };

        let long = "a".repeat(core_engine::store::MAX_PATTERN_BYTES + 1);
        let err = verify_sync_token(secret, &mint(&long))
            .expect_err("an over-long granted pattern must be refused");
        assert_eq!(err, "token grants an over-long pattern");

        // One over-long pattern in an otherwise fine list is still refused.
        assert!(verify_sync_token(secret, &mint(&format!("cart:*,{long}"))).is_err());

        // A pattern exactly at the cap is still honoured.
        let at_cap = "a".repeat(core_engine::store::MAX_PATTERN_BYTES);
        assert_eq!(
            verify_sync_token(secret, &mint(&at_cap)),
            Ok(vec![at_cap.clone()])
        );
        assert_eq!(
            verify_sync_token(secret, &mint("cart:42:*,user:1:*")),
            Ok(vec!["cart:42:*".to_string(), "user:1:*".to_string()])
        );
    }
}

// ── Expiry propagation ────────────────────────────────────────────────────────
