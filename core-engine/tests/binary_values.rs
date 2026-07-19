//! Tripwire for the engine's handling of values that are not valid UTF-8.
//!
//! `EntryValue::Str` is a `String`, so a value is forced through a lossy UTF-8
//! conversion at command-parse time. This is a property of the *engine*, not of
//! any transport: the round-trip below is lossy over TCP exactly as it is over
//! WebSocket, so the two are already equivalent — just equivalently lossy.
//!
//! This test asserts the current (lossy) behaviour deliberately. When byte-safe
//! values land, it will fail — flip it to assert the bytes survive.

use core_engine::{cmd::Command, resp::Value, store::KeyValueStore};

#[test]
fn non_utf8_values_are_lossily_replaced() {
    // 0xFF 0xFE is not valid UTF-8 in any position.
    let raw: &[u8] = b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$3\r\n\xff\xfe\x41\r\n";
    let (v, _) = Value::parse(raw).unwrap();
    let store = KeyValueStore::new();
    store.execute(Command::from_value(v).unwrap());

    let Value::BulkString(Some(got)) = store.execute(Command::Get("k".into())) else {
        panic!("key missing after SET");
    };

    // Each invalid byte becomes U+FFFD (EF BF BD), so 3 bytes in, 7 bytes out.
    assert_eq!(
        got,
        b"\xef\xbf\xbd\xef\xbf\xbd\x41".to_vec(),
        "engine is now byte-safe — update this test to assert the round-trip"
    );
}

#[test]
fn utf8_values_survive_unchanged() {
    let store = KeyValueStore::new();
    store.execute(Command::Set(
        "k".into(),
        "héllo ✓".into(),
        Default::default(),
    ));
    let Value::BulkString(Some(got)) = store.execute(Command::Get("k".into())) else {
        panic!("key missing after SET");
    };
    assert_eq!(String::from_utf8(got).unwrap(), "héllo ✓");
}
