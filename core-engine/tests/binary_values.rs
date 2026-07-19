//! Values that are not valid UTF-8 are rejected, not silently corrupted.
//!
//! `EntryValue::Str` is a `String`, so a non-UTF-8 value cannot be stored
//! faithfully. Until values are byte-transparent (see docs/roadmap.md), the
//! command is refused at parse time — before anything is written — rather than
//! being lossily converted to U+FFFD behind a successful `+OK`.

use core_engine::{cmd::Command, resp::Value, store::KeyValueStore};

/// `SET k <0xFF 0xFE 'A'>` — invalid UTF-8 in the value position.
const SET_BINARY_VALUE: &[u8] = b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$3\r\n\xff\xfe\x41\r\n";

#[test]
fn non_utf8_values_are_rejected() {
    let (v, _) = Value::parse(SET_BINARY_VALUE).unwrap();
    let err = Command::from_value(v).expect_err("binary value must be refused");
    assert!(err.starts_with("ERR "), "must be a RESP error: {err:?}");
    assert!(
        err.contains("base64"),
        "the error must say what to do instead: {err:?}"
    );
}

#[test]
fn a_rejected_command_stores_nothing() {
    // The dangerous failure was a silent partial success: OK returned, wrong
    // bytes stored. Nothing may reach the store.
    let store = KeyValueStore::new();
    let (v, _) = Value::parse(SET_BINARY_VALUE).unwrap();
    assert!(Command::from_value(v).is_err());
    assert_eq!(
        store.execute(Command::Get("k".into())),
        Value::BulkString(None)
    );
}

#[test]
fn non_utf8_keys_are_rejected_too() {
    // A key is as lossy as a value, and a corrupted key is unreachable
    // afterwards — the write would be silently unretrievable.
    let raw: &[u8] = b"*3\r\n$3\r\nSET\r\n$2\r\n\xff\xfe\r\n$1\r\nv\r\n";
    let (v, _) = Value::parse(raw).unwrap();
    assert!(Command::from_value(v).is_err());
}

#[test]
fn the_error_names_which_argument_was_bad() {
    // MSET k1 v1 k2 <binary> — the bad argument is index 4, not the first.
    let raw: &[u8] = b"*5\r\n$4\r\nMSET\r\n$2\r\nk1\r\n$2\r\nv1\r\n$2\r\nk2\r\n$2\r\n\xff\xfe\r\n";
    let (v, _) = Value::parse(raw).unwrap();
    let err = Command::from_value(v).expect_err("must be refused");
    assert!(err.contains("argument 4"), "got {err:?}");
}

#[test]
fn utf8_values_survive_unchanged() {
    // Multi-byte UTF-8 is valid and must not be caught by the new check.
    let store = KeyValueStore::new();
    for value in ["héllo ✓", "日本語", "\u{1F600}", "", "plain"] {
        store.execute(Command::Set("k".into(), value.into(), Default::default()));
        let Value::BulkString(Some(got)) = store.execute(Command::Get("k".into())) else {
            panic!("key missing after SET of {value:?}");
        };
        assert_eq!(String::from_utf8(got).unwrap(), value);
    }
}

#[test]
fn utf8_values_still_parse_from_the_wire() {
    // Guards against the validation being over-eager and rejecting valid input.
    let raw = "*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$9\r\n日本語\r\n".as_bytes();
    let (v, _) = Value::parse(raw).unwrap();
    let store = KeyValueStore::new();
    store.execute(Command::from_value(v).expect("valid UTF-8 must be accepted"));
    assert_eq!(
        store.execute(Command::Get("k".into())),
        Value::BulkString(Some("日本語".as_bytes().to_vec()))
    );
}
