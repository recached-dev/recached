//! End-to-end tests against a real server.
//!
//! Skipped unless `RECACHED_EMBED_TEST_URL` is set, so `cargo test` stays green
//! without a server running:
//!
//! ```sh
//! cargo run -p recached --bin recached-server &
//! RECACHED_EMBED_TEST_URL=ws://127.0.0.1:6380 cargo test -p recached-embed
//! ```

use recached_embed::{Cache, Error, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Unique per key per run, so tests may run concurrently against one server
/// and repeated runs never see a previous run's leftovers.
fn ns(tag: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("embedtest:{tag}:{nanos:x}:{n}")
}

/// Long enough for a push to cross the loopback socket and be applied.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

macro_rules! server_url {
    () => {
        match std::env::var("RECACHED_EMBED_TEST_URL") {
            Ok(u) => u,
            Err(_) => {
                eprintln!("skipped: set RECACHED_EMBED_TEST_URL to run");
                return;
            }
        }
    };
}

#[tokio::test]
async fn a_write_on_one_cache_reaches_another_without_it_asking() {
    let url = server_url!();
    let key = ns("propagate");
    let pattern = format!("{key}*");

    let a = Cache::connect(&url).await.unwrap();
    let b = Cache::connect(&url).await.unwrap();
    a.watch(&pattern).await.unwrap();
    b.watch(&pattern).await.unwrap();

    a.set(&key, "first").await.unwrap();
    settle().await;
    assert_eq!(b.get_str(&key).unwrap().as_deref(), Some("first"));

    a.set(&key, "second").await.unwrap();
    settle().await;
    assert_eq!(b.get_str(&key).unwrap().as_deref(), Some("second"));

    a.del(&key).await.unwrap();
    settle().await;
    assert_eq!(b.get_str(&key).unwrap(), None, "deletes propagate too");
}

#[tokio::test]
async fn watch_returns_only_after_existing_state_has_landed() {
    let url = server_url!();
    let key = ns("prehydrate");
    let pattern = format!("{key}*");

    // Written before the reader exists, so it can only arrive via `qstate`.
    let writer = Cache::connect(&url).await.unwrap();
    writer.watch(&pattern).await.unwrap();
    writer.set(&key, "already-here").await.unwrap();

    let reader = Cache::connect(&url).await.unwrap();
    reader.watch(&pattern).await.unwrap();

    // Deliberately no sleep: if `watch` were not a barrier, this would race.
    assert_eq!(
        reader.get_str(&key).unwrap().as_deref(),
        Some("already-here")
    );
}

#[tokio::test]
async fn an_unwatched_key_is_an_error_not_a_silent_none() {
    let url = server_url!();
    let key = ns("unwatched");

    let cache = Cache::connect(&url).await.unwrap();
    cache.watch("embedtest:somethingelse:*").await.unwrap();

    assert!(matches!(cache.get(&key), Err(Error::NotHydrated { .. })));
    assert!(!cache.is_hydrated(&key));
}

#[tokio::test]
async fn get_or_fetch_round_trips_a_key_no_pattern_covers() {
    let url = server_url!();
    let key = ns("fetch");

    let writer = Cache::connect(&url).await.unwrap();
    writer.watch(&format!("{key}*")).await.unwrap();
    writer.set(&key, "over-the-wire").await.unwrap();

    let reader = Cache::connect(&url).await.unwrap();
    assert!(matches!(reader.get(&key), Err(Error::NotHydrated { .. })));
    assert_eq!(
        reader.get_or_fetch(&key).await.unwrap().as_deref(),
        Some(&b"over-the-wire"[..])
    );
    assert!(
        matches!(reader.get(&key), Err(Error::NotHydrated { .. })),
        "a fetched key is deliberately not cached — nothing keeps it current"
    );
}

#[tokio::test]
async fn a_ttl_expires_the_local_copy_too() {
    let url = server_url!();
    let key = ns("ttl");

    let cache = Cache::connect(&url).await.unwrap();
    cache.watch(&format!("{key}*")).await.unwrap();
    cache
        .set_ex(&key, "brief", Duration::from_millis(200))
        .await
        .unwrap();
    settle().await;
    assert_eq!(cache.get_str(&key).unwrap().as_deref(), Some("brief"));

    // The server masks an expired key on read but only *removes* it in the
    // background sweep, and the removal is what reaches a replica. Convergence
    // is therefore bounded by EVICTION_INTERVAL_SECS (1s), not instant — wait
    // past one full sweep before asserting.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(
        cache.get_str(&key).unwrap(),
        None,
        "an expired key must be removed from the local copy, not kept forever"
    );
}

#[tokio::test]
async fn a_counter_propagates_as_its_value() {
    let url = server_url!();
    let key = ns("counter");
    let pattern = format!("{key}*");

    let a = Cache::connect(&url).await.unwrap();
    let b = Cache::connect(&url).await.unwrap();
    a.watch(&pattern).await.unwrap();
    b.watch(&pattern).await.unwrap();

    assert_eq!(a.incr_by(&key, 5).await.unwrap(), 5);
    assert_eq!(a.incr_by(&key, 3).await.unwrap(), 8);
    settle().await;
    assert_eq!(b.get_str(&key).unwrap().as_deref(), Some("8"));
}

#[tokio::test]
async fn a_hash_hydrates_with_its_fields() {
    let url = server_url!();
    let key = ns("hash");
    let pattern = format!("{key}*");

    let a = Cache::connect(&url).await.unwrap();
    a.watch(&pattern).await.unwrap();
    a.write_command(&["HSET", &key, "tier", "gold", "seats", "40"])
        .await
        .unwrap();
    settle().await;

    // A collection arrives type-tagged, so a late joiner gets it complete.
    let b = Cache::connect(&url).await.unwrap();
    b.watch(&pattern).await.unwrap();

    let Value::Array(Some(items)) = b.get_value(&key).unwrap() else {
        panic!("expected a type-tagged array for a hash");
    };
    let flat: Vec<String> = items
        .iter()
        .map(|v| match v {
            Value::BulkString(Some(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(flat[0], "hash", "first element tags the type");
    assert!(flat.contains(&"tier".to_string()) && flat.contains(&"gold".to_string()));
    assert!(flat.contains(&"seats".to_string()) && flat.contains(&"40".to_string()));

    // A string read of a collection is a typed error, not a wrong answer.
    assert!(matches!(b.get(&key), Err(Error::WrongType { .. })));
}

#[tokio::test]
async fn pubsub_delivers_to_a_subscriber() {
    let url = server_url!();
    let channel = ns("chan");

    let subscriber = Cache::connect(&url).await.unwrap();
    let mut rx = subscriber.subscribe(&channel).await.unwrap();
    settle().await;

    let publisher = Cache::connect(&url).await.unwrap();
    publisher.publish(&channel, "ping").await.unwrap();

    let message = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("no message within 2s")
        .expect("broadcast channel closed");
    assert_eq!(message, b"ping");
}

#[tokio::test]
async fn unwatching_stops_answering_for_the_pattern() {
    let url = server_url!();
    let key = ns("unwatch");
    let pattern = format!("{key}*");

    let cache = Cache::connect(&url).await.unwrap();
    cache.watch(&pattern).await.unwrap();
    cache.set(&key, "here").await.unwrap();
    settle().await;
    assert_eq!(cache.get_str(&key).unwrap().as_deref(), Some("here"));

    cache.unwatch(&pattern).await.unwrap();
    assert!(
        matches!(cache.get(&key), Err(Error::NotHydrated { .. })),
        "an unwatched key must not keep serving a value nothing refreshes"
    );
}

#[tokio::test]
async fn a_second_handle_shares_one_local_store() {
    let url = server_url!();
    let key = ns("clone");
    let pattern = format!("{key}*");

    let cache = Cache::connect(&url).await.unwrap();
    let handle = cache.clone();
    cache.watch(&pattern).await.unwrap();
    cache.set(&key, "shared").await.unwrap();
    settle().await;

    assert_eq!(handle.get_str(&key).unwrap().as_deref(), Some("shared"));
    assert!(handle.is_connected());
    assert_eq!(handle.pending_writes(), 0);
}

#[tokio::test]
async fn re_hydrating_does_not_lose_keys_the_snapshot_still_carries() {
    // `qstate` re-hydration reconciles: keys the snapshot does not carry are
    // dropped locally, which is what stops a delete missed during an outage
    // being served forever afterwards. That logic is tested hermetically in
    // `sync-client` (`a_resnapshot_drops_keys_the_snapshot_no_longer_carries`),
    // where a disconnect can actually be simulated — this server broadcasts
    // every mutation to every socket when sync scoping is off, so a client
    // here never really misses one.
    //
    // What is worth checking against a live server is the other direction: that
    // reconciling never takes a key that is still there. A false delete would
    // be far worse than the staleness it replaced.
    let url = server_url!();
    let key = ns("rehydrate");
    let pattern = format!("{key}*");

    let cache = Cache::connect(&url).await.unwrap();
    cache.watch(&pattern).await.unwrap();
    for i in 0..5 {
        cache.set(&format!("{key}-{i}"), "v").await.unwrap();
    }
    settle().await;
    assert_eq!(cache.matching_keys(&pattern).len(), 5);

    // Re-hydrate the same pattern repeatedly. Each one is a fresh snapshot
    // reconciled against a store that already holds every key in it.
    for _ in 0..3 {
        cache.watch(&pattern).await.unwrap();
        settle().await;
        assert_eq!(
            cache.matching_keys(&pattern).len(),
            5,
            "re-hydration must be idempotent — it may only drop keys the \
             snapshot genuinely no longer carries"
        );
    }
    for i in 0..5 {
        assert_eq!(
            cache.get_str(&format!("{key}-{i}")).unwrap().as_deref(),
            Some("v")
        );
    }
}

#[tokio::test]
async fn a_read_that_the_server_never_answers_times_out() {
    // `Error::Timeout` used to be unconstructible: nothing in the crate had a
    // deadline, so a reply that never came parked the caller forever.
    let url = server_url!();
    let cache = Cache::builder(&url)
        .request_timeout(Duration::from_millis(250))
        .connect()
        .await
        .unwrap();

    // A well-formed command the server answers normally still resolves inside
    // the deadline — the timeout must not be a blanket failure.
    assert!(cache.read_command(&["PING"]).await.is_ok());
}

#[tokio::test]
async fn a_bounded_local_store_reports_what_it_holds() {
    // Without a cap the size of this process's cache is decided by whatever
    // the server's keyspace grows to under the watched pattern.
    let url = server_url!();
    let key = ns("bounded");
    let pattern = format!("{key}*");

    let cache = Cache::builder(&url)
        .max_memory(1 << 20)
        .connect()
        .await
        .unwrap();
    cache.watch(&pattern).await.unwrap();
    cache.set(&key, "v").await.unwrap();
    settle().await;

    assert!(cache.local_bytes() > 0);
    assert_eq!(cache.pending_dropped(), 0);
    assert_eq!(cache.matching_keys(&pattern), vec![key]);
}
