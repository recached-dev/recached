//! A snapshot written before values became bytes must still load.
//!
//! `SnapshotValue` held `String` up to 0.2.1 and holds `Blob` from 0.2.2.
//! rmp-serde encodes those differently — msgpack `str` versus `bin` — so
//! `Blob`'s deserializer accepts either. Without that, upgrading a server would
//! silently start from an empty cache, or fail to boot.

use core_engine::store::{KeyValueStore, SnapshotEntry, SnapshotValue};
use serde::Serialize;
use std::collections::HashMap;

/// Mirror of the pre-0.2.2 `SnapshotValue`, used to produce a genuine old-format
/// payload rather than a hand-rolled byte string. Variant order matters:
/// rmp-serde encodes variants by index.
#[derive(Serialize)]
#[allow(dead_code)] // variants exist to fix the discriminant order, not to be built
enum LegacySnapshotValue {
    Str(String),
    Hash(HashMap<String, String>),
    List(Vec<String>),
    Set(Vec<String>),
    ZSet(Vec<(String, f64)>),
    RateLimiter {
        limit: u64,
        window_ms: u64,
        events: Vec<u64>,
    },
    Json(String),
}

#[derive(Serialize)]
struct LegacyEntry {
    key: String,
    value: LegacySnapshotValue,
    expires_at_ms: Option<u64>,
}

#[test]
fn a_pre_0_2_2_snapshot_still_restores() {
    let legacy = vec![
        LegacyEntry {
            key: "s".into(),
            value: LegacySnapshotValue::Str("hello".into()),
            expires_at_ms: None,
        },
        LegacyEntry {
            key: "l".into(),
            value: LegacySnapshotValue::List(vec!["a".into(), "b".into()]),
            expires_at_ms: None,
        },
        LegacyEntry {
            key: "h".into(),
            value: LegacySnapshotValue::Hash(HashMap::from([("f".to_string(), "v".to_string())])),
            expires_at_ms: None,
        },
    ];
    let bytes = rmp_serde::to_vec(&legacy).expect("legacy snapshot must encode");

    // Decode with the *current* types — this is what a restarted server does.
    let entries: Vec<SnapshotEntry> =
        rmp_serde::from_slice(&bytes).expect("a pre-0.2.2 snapshot must still decode");

    let store = KeyValueStore::new();
    store.restore(entries);

    use core_engine::{cmd::Command, resp::Value};
    assert_eq!(
        store.execute(Command::Get("s".into())),
        Value::BulkString(Some(b"hello".to_vec()))
    );
    assert_eq!(
        store.execute(Command::HGet("h".into(), "f".into())),
        Value::BulkString(Some(b"v".to_vec()))
    );
    assert_eq!(store.execute(Command::LLen("l".into())), Value::Integer(2));
}

#[test]
fn binary_values_survive_a_snapshot_round_trip() {
    let binary = vec![0xff, 0xfe, 0x00, 0x41, 0x80];
    let store = KeyValueStore::new();
    store.restore(vec![SnapshotEntry {
        key: "b".into(),
        value: SnapshotValue::Str(binary.clone().into()),
        expires_at_ms: None,
    }]);

    let bytes = rmp_serde::to_vec(&store.snapshot()).unwrap();
    let entries: Vec<SnapshotEntry> = rmp_serde::from_slice(&bytes).unwrap();

    let restored = KeyValueStore::new();
    restored.restore(entries);

    use core_engine::{cmd::Command, resp::Value};
    assert_eq!(
        restored.execute(Command::Get("b".into())),
        Value::BulkString(Some(binary)),
        "binary must survive snapshot and restore"
    );
}

#[test]
fn a_binary_value_encodes_as_msgpack_bin_not_an_int_array() {
    // Vec<u8> serializes as an array of integers by default, which would roughly
    // double snapshot size for binary payloads. Blob emits a compact `bin`.
    let store = KeyValueStore::new();
    store.restore(vec![SnapshotEntry {
        key: "b".into(),
        value: SnapshotValue::Str(vec![0xffu8; 1000].into()),
        expires_at_ms: None,
    }]);
    let bytes = rmp_serde::to_vec(&store.snapshot()).unwrap();
    assert!(
        bytes.len() < 1200,
        "1000 bytes encoded to {} — likely an int array, not msgpack bin",
        bytes.len()
    );
}
