use super::*;
use core_engine::cmd::Command;

fn client() -> SyncClient {
    SyncClient::new(Arc::new(KeyValueStore::new()), "client-a".to_string())
}

fn get(c: &SyncClient, key: &str) -> Value {
    c.store().execute(Command::Get(key.to_string()))
}

fn bulk(s: &str) -> Value {
    Value::BulkString(Some(s.as_bytes().to_vec()))
}

// ── dedup envelopes ───────────────────────────────────────────────────────────

#[test]
fn enqueue_wraps_writes_in_dedup_envelope() {
    let mut c = client();
    let e = c.enqueue_write(&to_resp(&["INCRBY", "n", "2"]), true, false);
    assert_eq!(e.id, 0);
    assert!(!e.send_now);
    assert_eq!(
        e.frame,
        "*6\r\n$5\r\nDEDUP\r\n$8\r\nclient-a\r\n$1\r\n0\r\n$6\r\nINCRBY\r\n$1\r\nn\r\n$1\r\n2\r\n"
    );
    // Ids are monotonic.
    let e2 = c.enqueue_write(&to_resp(&["SET", "k", "v"]), true, false);
    assert_eq!(e2.id, 1);
    assert!(e2.frame.contains("$1\r\n1\r\n"));
}

#[test]
fn epoch_forms_upper_bits_of_wire_id() {
    let mut c = client();
    c.set_epoch(2);
    let e = c.enqueue_write(&to_resp(&["SET", "k", "v"]), true, false);
    let wire_id = (2u64 << 32).to_string();
    assert!(e.frame.contains(&wire_id), "frame: {}", e.frame);
}

#[test]
fn nodedup_writes_pass_through_unwrapped() {
    let mut c = client();
    let plain = to_resp(&["SUBSCRIBE", "news"]);
    let e = c.enqueue_write(&plain, false, false);
    assert_eq!(e.frame, plain);
}

// ── connection lifecycle & replay order ───────────────────────────────────────

#[test]
fn on_open_replays_session_then_outbox_in_order() {
    let mut c = client();
    assert!(c.set_password("pw", false).is_none());
    assert!(c.set_sync_token("tok", false).is_none());
    assert!(c.add_live_query("cart:*", false).is_none());
    let w1 = c.enqueue_write(&to_resp(&["SET", "a", "1"]), true, false);
    let w2 = c.enqueue_write(&to_resp(&["SET", "b", "2"]), true, false);

    let frames = c.on_open();
    assert_eq!(frames.len(), 5);
    assert!(frames[0].contains("AUTH"));
    assert!(frames[1].contains("TOKEN"));
    assert!(frames[2].contains("QSUB"));
    assert_eq!(frames[3], w1.frame);
    assert_eq!(frames[4], w2.frame);
}

#[test]
fn session_commands_sent_while_open_occupy_reply_slots() {
    let mut c = client();
    // Open with nothing queued.
    assert!(c.on_open().is_empty());
    // A session command on the live socket...
    let frame = c.set_sync_token("tok", true);
    assert!(frame.is_some());
    // ...must consume the next reply without retiring any outbox row.
    let w = c.enqueue_write(&to_resp(&["SET", "a", "1"]), true, true);
    assert!(w.send_now);
    assert_eq!(
        c.handle_frame("*1\r\n$6\r\ncart:*\r\n"),
        Incoming::Reply { retired: None }
    );
    assert_eq!(
        c.handle_frame("+OK\r\n"),
        Incoming::Reply {
            retired: Some(w.id)
        }
    );
    assert_eq!(c.outbox_len(), 0);
}

#[test]
fn backoff_doubles_and_caps() {
    // Jitter puts each delay in [nominal/2, nominal]; assert the band rather
    // than an exact value.
    fn band(delay: u32, nominal: u32) {
        assert!(
            delay >= nominal / 2 && delay <= nominal,
            "delay {delay} outside [{}, {nominal}]",
            nominal / 2
        );
    }

    let mut c = client();
    band(c.on_close(), 500);
    band(c.on_close(), 1000);
    band(c.on_close(), 2000);
    for _ in 0..10 {
        c.on_close();
    }
    band(c.on_close(), 30_000);
    // A successful open resets the schedule.
    c.on_open();
    band(c.on_close(), 500);
}

#[test]
fn backoff_is_jittered_across_clients() {
    // The point of jitter: two clients disconnected by the same event must not
    // compute the same schedule, or they reconnect in lockstep.
    let mut a = SyncClient::new(Arc::new(KeyValueStore::new()), "client-a".to_string());
    let mut b = SyncClient::new(Arc::new(KeyValueStore::new()), "client-b".to_string());

    let seq_a: Vec<u32> = (0..6).map(|_| a.on_close()).collect();
    let seq_b: Vec<u32> = (0..6).map(|_| b.on_close()).collect();
    assert_ne!(seq_a, seq_b, "distinct clients must not share a schedule");
}

#[test]
fn backoff_never_returns_zero_or_exceeds_the_cap() {
    // A zero delay would busy-loop reconnects; exceeding the cap would strand
    // a client for longer than intended.
    let mut c = client();
    for _ in 0..40 {
        let d = c.on_close();
        assert!(d > 0, "backoff must never be zero");
        assert!(d <= BACKOFF_CAP_MS, "backoff {d} exceeded the cap");
    }
}

#[test]
fn jitter_spreads_reconnects_over_the_window() {
    // With many clients the delays should occupy a range, not cluster on one
    // value — that is the property that prevents the herd.
    let delays: Vec<u32> = (0..50)
        .map(|i| {
            let mut c = SyncClient::new(Arc::new(KeyValueStore::new()), format!("client-{i}"));
            for _ in 0..3 {
                c.on_close();
            }
            c.on_close() // nominal 4000ms
        })
        .collect();

    let unique: std::collections::HashSet<u32> = delays.iter().copied().collect();
    assert!(
        unique.len() > 20,
        "expected a spread of delays, got {} distinct values",
        unique.len()
    );
    assert!(delays.iter().all(|d| *d >= 2000 && *d <= 4000));
}

// ── acknowledgment & retirement ───────────────────────────────────────────────

#[test]
fn replies_retire_outbox_rows_in_order() {
    let mut c = client();
    let w1 = c.enqueue_write(&to_resp(&["SET", "a", "1"]), true, false);
    let w2 = c.enqueue_write(&to_resp(&["SET", "b", "2"]), true, false);
    let frames = c.on_open();
    assert_eq!(frames.len(), 2);

    assert_eq!(
        c.handle_frame("+OK\r\n"),
        Incoming::Reply {
            retired: Some(w1.id)
        }
    );
    assert_eq!(c.outbox_len(), 1);
    // +DUP is a reply like any other — the row still retires.
    assert_eq!(
        c.handle_frame("+DUP\r\n"),
        Incoming::Reply {
            retired: Some(w2.id)
        }
    );
    assert_eq!(c.outbox_len(), 0);
}

#[test]
fn unacked_rows_survive_reconnect_and_replay() {
    let mut c = client();
    let w = c.enqueue_write(&to_resp(&["INCRBY", "n", "1"]), true, false);
    assert_eq!(c.on_open().len(), 1);
    // Connection dies before the reply arrives.
    c.on_close();
    // The row is still queued; the next open replays the identical frame.
    let frames = c.on_open();
    assert_eq!(frames, vec![w.frame]);
}

#[test]
fn pushes_are_not_replies() {
    let mut c = client();
    let w = c.enqueue_write(&to_resp(&["SET", "a", "1"]), true, false);
    c.on_open();
    // A mutation push and a keychange arrive before our reply — neither may
    // consume the reply slot.
    assert_eq!(
        c.handle_frame(">3\r\n$3\r\nSET\r\n$1\r\nx\r\n$1\r\ny\r\n"),
        Incoming::Applied
    );
    assert_eq!(
        c.handle_frame("*3\r\n$9\r\nkeychange\r\n$1\r\nx\r\n$1\r\nz\r\n"),
        Incoming::Applied
    );
    assert_eq!(
        c.handle_frame("+OK\r\n"),
        Incoming::Reply {
            retired: Some(w.id)
        }
    );
}

#[test]
fn outbox_overflow_drops_oldest() {
    let mut c = client();
    let first = c.enqueue_write(&to_resp(&["SET", "k0", "v"]), true, false);
    for i in 1..MAX_PENDING_WRITES {
        c.enqueue_write(&to_resp(&["SET", &format!("k{i}"), "v"]), true, false);
    }
    let overflow = c.enqueue_write(&to_resp(&["SET", "last", "v"]), true, false);
    assert_eq!(overflow.dropped, Some(first.id));
    assert_eq!(c.outbox_len(), MAX_PENDING_WRITES);
}

// ── frame application ─────────────────────────────────────────────────────────

#[test]
fn qstate_applies_state_and_counts_as_reply() {
    let mut c = client();
    assert!(c.add_live_query("cart:*", false).is_none());
    c.on_open();
    let qstate = "*4\r\n$6\r\nqstate\r\n$6\r\ncart:*\r\n$6\r\ncart:1\r\n$5\r\napple\r\n";
    assert_eq!(
        c.handle_frame(qstate),
        Incoming::AppliedReply { retired: None }
    );
    assert_eq!(get(&c, "cart:1"), bulk("apple"));
}

#[test]
fn keychange_sets_and_deletes() {
    let mut c = client();
    c.handle_frame("*3\r\n$9\r\nkeychange\r\n$1\r\nk\r\n$1\r\nv\r\n");
    assert_eq!(get(&c, "k"), bulk("v"));
    c.handle_frame("*3\r\n$9\r\nkeychange\r\n$1\r\nk\r\n$-1\r\n");
    assert_eq!(get(&c, "k"), Value::BulkString(None));
}

#[test]
fn pubsub_messages_surface_without_touching_store() {
    let mut c = client();
    let msg = c.handle_frame(">3\r\n$7\r\nmessage\r\n$4\r\nnews\r\n$5\r\nhello\r\n");
    assert_eq!(
        msg,
        Incoming::PubSub {
            channel: "news".into(),
            message: "hello".into()
        }
    );
}

// ── identity & restore ────────────────────────────────────────────────────────

#[test]
fn client_id_adoption_only_before_writes() {
    let mut c = client();
    assert!(c.adopt_client_id("persisted-id".into()));
    assert_eq!(c.client_id(), "persisted-id");
    c.enqueue_write(&to_resp(&["SET", "a", "1"]), true, false);
    assert!(!c.adopt_client_id("too-late".into()));
    assert_eq!(c.client_id(), "persisted-id");
}

#[test]
fn restore_renumbers_but_preserves_frames_and_order() {
    let mut c = client();
    // This session already queued one write before restore (connect-first flow).
    let session_write = c.enqueue_write(&to_resp(&["SET", "new", "1"]), true, false);
    // Two rows from the previous session, stored under old ids.
    let old_frame_a = "*5\r\n$5\r\nDEDUP\r\n$8\r\nclient-a\r\n$1\r\n7\r\n$3\r\nDEL\r\n$1\r\na\r\n";
    let old_frame_b = "*5\r\n$5\r\nDEDUP\r\n$8\r\nclient-a\r\n$1\r\n8\r\n$3\r\nDEL\r\n$1\r\nb\r\n";
    let rewrites = c.restore_outbox(vec![(8, old_frame_b.into()), (7, old_frame_a.into())]);

    // New ids never collide with the session write's id.
    for (_, new_id, _) in &rewrites {
        assert_ne!(*new_id, session_write.id);
    }
    // Replay order: restored rows first (oldest first), then this session's.
    let frames = c.on_open();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0], old_frame_a);
    assert_eq!(frames[1], old_frame_b);
    assert_eq!(frames[2], session_write.frame);
}

// ── Sync scopes ───────────────────────────────────────────────────────────────

#[test]
fn sync_scopes_builds_a_sync_frame_from_csv() {
    let mut c = client();
    let frame = c.set_sync_scopes("cart:*,user:1:*", true).unwrap();
    assert!(frame.contains("SYNC"), "{frame}");
    assert!(frame.contains("cart:*"));
    assert!(frame.contains("user:1:*"));
}

#[test]
fn sync_scopes_ignore_blank_entries_and_whitespace() {
    let mut c = client();
    let frame = c.set_sync_scopes(" cart:* , , user:1:* ,", true).unwrap();
    // Three commas but only two real patterns → SYNC + 2 args.
    assert!(frame.starts_with("*3\r\n"), "expected 3 parts, got {frame}");
    assert!(frame.contains("cart:*") && frame.contains("user:1:*"));
}

#[test]
fn an_all_empty_scope_list_produces_no_frame() {
    // Sending a bare SYNC would *clear* scopes on the server, which is the
    // opposite of what an accidental empty string intends.
    let mut c = client();
    assert!(c.set_sync_scopes("", true).is_none());
    assert!(c.set_sync_scopes("  ,  , ", true).is_none());
}

#[test]
fn scopes_are_replayed_on_reconnect() {
    let mut c = client();
    c.set_sync_scopes("cart:*", false);
    let frames = c.on_open();
    assert!(
        frames.iter().any(|f| f.contains("cart:*")),
        "scopes must be re-established after reconnect: {frames:?}"
    );
}

#[test]
fn a_sync_token_takes_precedence_over_raw_scope_patterns() {
    // The token carries server-signed scopes; sending raw patterns as well
    // would be redundant and could widen what the connection asks for.
    let mut c = client();
    c.set_sync_scopes("cart:*", false);
    c.set_sync_token("tok-123", false);
    let frames = c.on_open();
    assert!(frames.iter().any(|f| f.contains("tok-123")));
    assert!(
        !frames.iter().any(|f| f.contains("cart:*")),
        "raw scopes must not be sent alongside a token: {frames:?}"
    );
}

// ── Live queries ──────────────────────────────────────────────────────────────

#[test]
fn live_queries_register_once_and_replay_on_open() {
    let mut c = client();
    c.add_live_query("cart:*", false);
    c.add_live_query("cart:*", false); // idempotent
    c.add_live_query("user:*", false);

    let frames = c.on_open();
    let qsubs = frames.iter().filter(|f| f.contains("QSUB")).count();
    assert_eq!(
        qsubs, 2,
        "duplicate patterns must not double-subscribe: {frames:?}"
    );
}

#[test]
fn removing_one_live_query_leaves_the_others() {
    let mut c = client();
    c.add_live_query("cart:*", false);
    c.add_live_query("user:*", false);

    let frame = c.remove_live_query(Some("cart:*"), true).unwrap();
    assert!(
        frame.contains("QUNSUB") && frame.contains("cart:*"),
        "{frame}"
    );

    let frames = c.on_open();
    assert!(
        frames.iter().any(|f| f.contains("user:*")),
        "survivor replays"
    );
    assert!(
        !frames.iter().any(|f| f.contains("cart:*")),
        "removed query must not replay: {frames:?}"
    );
}

#[test]
fn removing_all_live_queries_sends_a_bare_qunsub() {
    let mut c = client();
    c.add_live_query("cart:*", false);
    c.add_live_query("user:*", false);

    let frame = c.remove_live_query(None, true).unwrap();
    assert!(frame.contains("QUNSUB"), "{frame}");
    assert!(
        !frame.contains("cart:*"),
        "bare QUNSUB carries no pattern: {frame}"
    );

    let frames = c.on_open();
    assert!(
        !frames.iter().any(|f| f.contains("QSUB")),
        "nothing should replay after clearing: {frames:?}"
    );
}

// ── Session frame ordering ────────────────────────────────────────────────────

#[test]
fn on_open_sends_auth_before_scopes_before_queries() {
    // Order is load-bearing: the server rejects scoped commands until it has
    // authenticated, and a QSUB before SYNC would be refused.
    let mut c = client();
    c.set_password("pw", false);
    c.set_sync_token("tok", false);
    c.add_live_query("cart:*", false);

    let frames = c.on_open();
    let pos = |needle: &str| frames.iter().position(|f| f.contains(needle));
    let (auth, sync, qsub) = (
        pos("AUTH").unwrap(),
        pos("TOKEN").unwrap(),
        pos("QSUB").unwrap(),
    );
    assert!(auth < sync, "AUTH must precede SYNC: {frames:?}");
    assert!(sync < qsub, "SYNC must precede QSUB: {frames:?}");
}

#[test]
fn on_open_resets_attempt_count_and_inflight() {
    let mut c = client();
    c.on_close();
    c.on_close();
    c.enqueue_write(&to_resp(&["SET", "k", "v"]), true, true);

    c.on_open();
    // Backoff restarts from the floor after a successful open (jittered).
    let d = c.on_close();
    assert!((250..=500).contains(&d), "expected a reset floor, got {d}");
}

#[test]
fn session_frames_are_withheld_until_connected() {
    let mut c = client();
    assert!(
        c.add_live_query("cart:*", false).is_none(),
        "nothing to send while disconnected — it replays on open instead"
    );
    assert!(
        c.add_live_query("user:*", true).is_some(),
        "sent immediately when open"
    );
}

// ── Outbox management ─────────────────────────────────────────────────────────

#[test]
fn clear_outbox_drops_queued_and_inflight_writes() {
    let mut c = client();
    c.enqueue_write(&to_resp(&["SET", "a", "1"]), true, true);
    c.enqueue_write(&to_resp(&["SET", "b", "2"]), true, true);
    assert_eq!(c.outbox_len(), 2);

    c.clear_outbox();
    assert_eq!(c.outbox_len(), 0);
    // A reply arriving after a clear must not retire a row that no longer
    // exists or panic on an empty inflight queue.
    c.handle_frame("+OK\r\n");
    assert_eq!(c.outbox_len(), 0);
}

#[test]
fn no_writes_yet_tracks_whether_an_identity_can_still_be_adopted() {
    let mut c = client();
    assert!(c.no_writes_yet(), "fresh client has written nothing");
    c.enqueue_write(&to_resp(&["SET", "k", "v"]), true, true);
    assert!(
        !c.no_writes_yet(),
        "after a write the client id is committed to the wire"
    );
}

#[test]
fn epoch_is_readable_and_settable() {
    let mut c = client();
    assert_eq!(c.epoch(), 0);
    c.set_epoch(7);
    assert_eq!(c.epoch(), 7);
}

// ── Type-tagged collection values ─────────────────────────────────────────────
// A live query used to deliver only a type name for collections, forcing the
// subscriber into a follow-up HGETALL/LRANGE — a network round-trip in a system
// built on local reads. Values now arrive tagged and complete.

/// Build the keychange frame a server sends for `key`, using the real
/// `get_current` encoding rather than a hand-written fixture.
fn keychange_frame(source: &KeyValueStore, key: &str) -> Vec<Value> {
    vec![
        Value::BulkString(Some(b"keychange".to_vec())),
        Value::BulkString(Some(key.as_bytes().to_vec())),
        source.get_current(key),
    ]
}

/// keychange/qstate frames are plain RESP arrays, matching the wire format
/// the existing tests use.
fn push(items: Vec<Value>) -> String {
    String::from_utf8_lossy(&Value::Array(Some(items)).serialize()).into_owned()
}

#[test]
fn keychange_rebuilds_a_hash_without_a_re_read() {
    let source = KeyValueStore::new();
    source.execute(Command::HSet(
        "cart:42".into(),
        vec![("item".into(), "book".into()), ("qty".into(), "2".into())],
    ));

    let mut c = client();
    c.handle_frame(&push(keychange_frame(&source, "cart:42")));

    assert_eq!(
        c.store()
            .execute(Command::HGet("cart:42".into(), "item".into())),
        bulk("book")
    );
    assert_eq!(
        c.store()
            .execute(Command::HGet("cart:42".into(), "qty".into())),
        bulk("2")
    );
}

#[test]
fn keychange_rebuilds_lists_in_order() {
    let source = KeyValueStore::new();
    source.execute(Command::RPush(
        "queue".into(),
        vec!["first".into(), "second".into(), "third".into()],
    ));

    let mut c = client();
    c.handle_frame(&push(keychange_frame(&source, "queue")));

    assert_eq!(
        c.store().execute(Command::LRange("queue".into(), 0, -1)),
        Value::Array(Some(vec![bulk("first"), bulk("second"), bulk("third")])),
        "list order must survive the round trip"
    );
}

#[test]
fn keychange_rebuilds_sets_and_zsets() {
    let source = KeyValueStore::new();
    source.execute(Command::SAdd(
        "tags".into(),
        vec!["red".into(), "blue".into()],
    ));
    source.execute(Command::ZAdd(
        "board".into(),
        Default::default(),
        vec![(10.0, "alice".into()), (5.0, "bob".into())],
    ));

    let mut c = client();
    c.handle_frame(&push(keychange_frame(&source, "tags")));
    c.handle_frame(&push(keychange_frame(&source, "board")));

    assert_eq!(
        c.store().execute(Command::SCard("tags".into())),
        Value::Integer(2)
    );
    assert_eq!(
        c.store()
            .execute(Command::ZScore("board".into(), "alice".into())),
        bulk("10")
    );
}

#[test]
fn keychange_rebuilds_json_documents() {
    let source = KeyValueStore::new();
    source.execute(Command::JSet(
        "doc".into(),
        "$".into(),
        "{\"a\":1}".to_string(),
    ));

    let mut c = client();
    c.handle_frame(&push(keychange_frame(&source, "doc")));

    assert_eq!(
        c.store().execute(Command::JGet("doc".into(), None)),
        bulk("{\"a\":1}")
    );
}

#[test]
fn a_removed_member_disappears_from_the_local_copy() {
    // The frame carries the complete value, so the key is rebuilt rather than
    // merged — otherwise a removal would never propagate.
    let source = KeyValueStore::new();
    source.execute(Command::SAdd(
        "tags".into(),
        vec!["red".into(), "blue".into()],
    ));

    let mut c = client();
    c.handle_frame(&push(keychange_frame(&source, "tags")));
    assert_eq!(
        c.store().execute(Command::SCard("tags".into())),
        Value::Integer(2)
    );

    source.execute(Command::SRem("tags".into(), vec!["red".into()]));
    c.handle_frame(&push(keychange_frame(&source, "tags")));

    assert_eq!(
        c.store()
            .execute(Command::SIsMember("tags".into(), "red".into())),
        Value::Integer(0),
        "a member removed on the server must not linger locally"
    );
    assert_eq!(
        c.store().execute(Command::SCard("tags".into())),
        Value::Integer(1)
    );
}

#[test]
fn qstate_delivers_complete_collections_on_subscribe() {
    // The initial state of a live query is now usable immediately.
    let source = KeyValueStore::new();
    source.execute(Command::HSet(
        "cart:1".into(),
        vec![("item".into(), "pen".into())],
    ));
    source.execute(Command::RPush("cart:2".into(), vec!["a".into()]));

    let mut c = client();
    let frame = push(vec![
        Value::BulkString(Some(b"qstate".to_vec())),
        Value::BulkString(Some(b"cart:*".to_vec())),
        Value::BulkString(Some(b"cart:1".to_vec())),
        source.get_current("cart:1"),
        Value::BulkString(Some(b"cart:2".to_vec())),
        source.get_current("cart:2"),
    ]);
    c.handle_frame(&frame);

    assert_eq!(
        c.store()
            .execute(Command::HGet("cart:1".into(), "item".into())),
        bulk("pen")
    );
    assert_eq!(
        c.store().execute(Command::LLen("cart:2".into())),
        Value::Integer(1)
    );
}

#[test]
fn an_unknown_type_tag_is_ignored_rather_than_guessed() {
    // Forward compatibility: a newer server sending a type this client does not
    // know must not corrupt the local copy.
    let mut c = client();
    c.handle_frame(&push(vec![
        Value::BulkString(Some(b"keychange".to_vec())),
        Value::BulkString(Some(b"k".to_vec())),
        Value::Array(Some(vec![
            Value::BulkString(Some(b"futuretype".to_vec())),
            Value::BulkString(Some(b"payload".to_vec())),
        ])),
    ]));
    assert_eq!(get(&c, "k"), Value::BulkString(None));
}
