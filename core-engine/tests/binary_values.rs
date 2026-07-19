//! Values are byte-transparent; identifiers are text.
//!
//! A stored value may be arbitrary bytes — compressed blobs, protobuf, images —
//! and must come back exactly as it went in. Keys, hash fields, set and
//! sorted-set members and glob patterns are looked up and matched as text, so a
//! non-UTF-8 one is refused rather than lossily converted.

use core_engine::{cmd::Command, resp::Value, store::KeyValueStore};

/// Invalid UTF-8 in any position: a lone continuation byte and a truncated
/// sequence, plus an embedded NUL and a byte that is legal only inside one.
const BINARY: &[u8] = &[0xff, 0xfe, 0x00, 0x41, 0x80, 0xc3];

fn parse(raw: &[u8]) -> Result<Command, String> {
    let (v, _) = Value::parse(raw).unwrap();
    Command::from_value(v)
}

/// Build a RESP array frame from raw byte arguments.
fn frame(args: &[&[u8]]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", args.len()).into_bytes();
    for a in args {
        out.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
        out.extend_from_slice(a);
        out.extend_from_slice(b"\r\n");
    }
    out
}

#[test]
fn a_binary_value_round_trips_byte_for_byte() {
    let store = KeyValueStore::new();
    store.execute(parse(&frame(&[b"SET", b"k", BINARY])).unwrap());

    let Value::BulkString(Some(got)) = store.execute(Command::Get("k".into())) else {
        panic!("key missing after SET");
    };
    assert_eq!(got, BINARY, "value must survive unchanged");
}

#[test]
fn binary_values_work_in_lists_and_hashes() {
    let store = KeyValueStore::new();

    store.execute(parse(&frame(&[b"RPUSH", b"l", BINARY, b"plain"])).unwrap());
    let Value::Array(Some(items)) = store.execute(Command::LRange("l".into(), 0, -1)) else {
        panic!("list missing");
    };
    assert_eq!(items[0], Value::BulkString(Some(BINARY.to_vec())));

    store.execute(parse(&frame(&[b"HSET", b"h", b"f", BINARY])).unwrap());
    assert_eq!(
        store.execute(Command::HGet("h".into(), "f".into())),
        Value::BulkString(Some(BINARY.to_vec()))
    );
}

#[test]
fn append_concatenates_bytes_rather_than_text() {
    let store = KeyValueStore::new();
    store.execute(parse(&frame(&[b"SET", b"k", BINARY])).unwrap());
    store.execute(parse(&frame(&[b"APPEND", b"k", BINARY])).unwrap());

    let Value::BulkString(Some(got)) = store.execute(Command::Get("k".into())) else {
        panic!("key missing");
    };
    assert_eq!(got.len(), BINARY.len() * 2);
    assert_eq!(&got[..BINARY.len()], BINARY);
    assert_eq!(&got[BINARY.len()..], BINARY);
}

#[test]
fn strlen_counts_bytes_not_characters() {
    let store = KeyValueStore::new();
    store.execute(parse(&frame(&[b"SET", b"k", BINARY])).unwrap());
    assert_eq!(
        store.execute(Command::Strlen("k".into())),
        Value::Integer(BINARY.len() as i64)
    );
}

#[test]
fn incr_on_a_binary_value_errors_like_any_non_numeric_value() {
    // The bytes are stored faithfully; they are simply not a number. This must
    // read as a type error, not as corruption.
    let store = KeyValueStore::new();
    store.execute(parse(&frame(&[b"SET", b"k", BINARY])).unwrap());
    let Value::Error(e) = store.execute(Command::Incr("k".into())) else {
        panic!("INCR on binary must error");
    };
    assert!(e.contains("not an integer"), "got {e:?}");
}

#[test]
fn a_binary_key_is_rejected() {
    // Keys are matched by glob and checked against sync scopes as text, so a
    // corrupted key would be silently unretrievable.
    let err = parse(&frame(&[b"SET", BINARY, b"v"])).expect_err("binary key must be refused");
    assert!(err.starts_with("ERR "), "{err:?}");
    assert!(err.contains("must be text"), "{err:?}");
}

#[test]
fn binary_fields_and_members_are_rejected() {
    for args in [
        vec![b"HSET".as_slice(), b"h", BINARY, b"v"], // hash field
        vec![b"SADD".as_slice(), b"s", BINARY],       // set member
        vec![b"ZADD".as_slice(), b"z", b"1", BINARY], // zset member
        vec![b"KEYS".as_slice(), BINARY],             // glob pattern
    ] {
        let err = parse(&frame(&args)).expect_err("identifier must be refused");
        assert!(err.contains("must be text"), "{:?} -> {err:?}", args[0]);
    }
}

#[test]
fn the_error_names_which_argument_was_bad() {
    // MSET k1 v1 <binary-key> v2 — index 3 is a key, so it is refused.
    let err = parse(&frame(&[b"MSET", b"k1", b"v1", BINARY, b"v2"])).expect_err("must be refused");
    assert!(err.contains("argument 3"), "got {err:?}");
}

#[test]
fn a_rejected_command_stores_nothing() {
    let store = KeyValueStore::new();
    assert!(parse(&frame(&[b"SET", BINARY, b"v"])).is_err());
    assert_eq!(store.execute(Command::DbSize), Value::Integer(0));
}

#[test]
fn utf8_values_survive_unchanged() {
    let store = KeyValueStore::new();
    for value in ["héllo ✓", "日本語", "\u{1F600}", "", "plain"] {
        store.execute(Command::Set("k".into(), value.into(), Default::default()));
        let Value::BulkString(Some(got)) = store.execute(Command::Get("k".into())) else {
            panic!("key missing after SET of {value:?}");
        };
        assert_eq!(String::from_utf8(got).unwrap(), value);
    }
}
