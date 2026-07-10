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
    let mut c = client();
    assert_eq!(c.on_close(), 500);
    assert_eq!(c.on_close(), 1000);
    assert_eq!(c.on_close(), 2000);
    for _ in 0..10 {
        c.on_close();
    }
    assert_eq!(c.on_close(), 30_000);
    // A successful open resets the schedule.
    c.on_open();
    assert_eq!(c.on_close(), 500);
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
