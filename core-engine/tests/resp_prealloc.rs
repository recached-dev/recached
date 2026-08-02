//! Allocation bounds for the RESP parser.
//!
//! A RESP aggregate header declares how many elements follow, and the header
//! arrives before any of them. Reserving capacity for the declared count let
//! nine bytes of input (`*1000000\r\n`) demand a multi-megabyte allocation, and
//! nesting multiplied it. None of it counted against `RECACHED_MAX_MEMORY`,
//! which tracks stored data only.
//!
//! This lives in its own integration test rather than in `resp.rs` because it
//! installs a `#[global_allocator]`. As a separate test binary that instrumented
//! allocator applies only here, and cannot perturb the several hundred unit
//! tests in the library.

use core_engine::resp::Value;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    /// Bytes requested while tracking is on, for this thread only.
    ///
    /// Thread-local rather than a global atomic because the test harness runs
    /// tests on parallel threads, and a shared counter would measure whatever
    /// else happened to be running.
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
    static TRACKING: Cell<bool> = const { Cell::new(false) };
}

struct CountingAlloc;

// SAFETY: every call delegates to the system allocator; the bookkeeping is a
// pair of `Cell`s in const-initialised thread-local storage, which performs no
// allocation of its own and so cannot re-enter. `try_with` rather than `with`
// because TLS is unavailable during thread teardown, where the accounting simply
// does not matter.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = TRACKING.try_with(|tracking| {
            if tracking.get() {
                let _ = ALLOCATED.try_with(|n| n.set(n.get() + layout.size()));
            }
        });
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Bytes requested by the allocator while `f` ran on this thread.
fn bytes_allocated(f: impl FnOnce()) -> usize {
    ALLOCATED.with(|n| n.set(0));
    TRACKING.with(|t| t.set(true));
    f();
    TRACKING.with(|t| t.set(false));
    ALLOCATED.with(|n| n.get())
}

/// Generous ceiling: the parser reserves a fixed 1024-element floor, which is
/// tens of kilobytes depending on `size_of::<Value>()`. The point is the two
/// orders of magnitude between this and the declared count, not a tight bound.
const BUDGET: usize = 128 * 1024;

#[test]
fn the_counting_allocator_actually_observes_allocations() {
    // If the harness silently failed to install, every other assertion here
    // would pass by measuring zero. Check the instrument before trusting it.
    let observed = bytes_allocated(|| {
        let v: Vec<u64> = Vec::with_capacity(4096);
        std::hint::black_box(&v);
    });
    assert!(
        observed >= 4096 * 8,
        "allocator not tracking: observed {observed} bytes for a 32 KB reservation"
    );
}

#[test]
fn a_declared_element_count_does_not_drive_the_reservation() {
    // Nine bytes. Before the cap this reserved a million `Value`s.
    let frame = b"*1000000\r\n";
    let used = bytes_allocated(|| {
        let _ = Value::parse(frame);
    });
    assert!(
        used < BUDGET,
        "parsing {} bytes allocated {used} bytes — the declared count is still driving the \
         reservation",
        frame.len()
    );
}

#[test]
fn the_same_applies_to_push_frames_and_maps() {
    for frame in [b">1000000\r\n".as_slice(), b"%500000\r\n".as_slice()] {
        let used = bytes_allocated(|| {
            let _ = Value::parse(frame);
        });
        assert!(
            used < BUDGET,
            "{:?} allocated {used} bytes",
            String::from_utf8_lossy(frame)
        );
    }
}

#[test]
fn nesting_does_not_multiply_the_reservation() {
    // One header per level, each declaring a million elements, to the depth
    // limit. Previously this was the declared count times the depth.
    let mut frame = Vec::new();
    for _ in 0..16 {
        frame.extend_from_slice(b"*1000000\r\n");
    }
    let used = bytes_allocated(|| {
        let _ = Value::parse(&frame);
    });
    assert!(
        used < 16 * BUDGET,
        "{} bytes of nested headers allocated {used} bytes",
        frame.len()
    );
}

#[test]
fn a_dripped_frame_costs_the_same_on_every_reparse() {
    // An incomplete frame is re-parsed from the start whenever more bytes
    // arrive, so the per-call reservation is paid per packet. One byte per
    // packet was the cheapest way to make the server allocate repeatedly.
    let frame = b"*1000000\r\n$1\r\na\r\n";
    let mut worst = 0usize;
    for cut in 1..frame.len() {
        let prefix = &frame[..cut];
        let used = bytes_allocated(|| {
            let _ = Value::parse(prefix);
        });
        worst = worst.max(used);
    }
    assert!(
        worst < BUDGET,
        "the most expensive prefix allocated {worst} bytes"
    );
}

#[test]
fn capping_the_reservation_did_not_cap_the_frame() {
    // The reservation is a floor, not a limit: a real aggregate larger than the
    // floor must still parse in full, with the vector growing as it goes.
    // Getting this wrong would silently truncate large pipelines and MSETs.
    const N: usize = 5_000;
    let mut frame = format!("*{N}\r\n").into_bytes();
    for i in 0..N {
        frame.extend_from_slice(format!("${}\r\n{}\r\n", i.to_string().len(), i).as_bytes());
    }

    let (parsed, consumed) = Value::parse(&frame).expect("a 5000-element array must parse");
    assert_eq!(consumed, frame.len(), "must consume the whole frame");
    match parsed {
        Value::Array(Some(items)) => {
            assert_eq!(items.len(), N, "every element must survive");
            assert_eq!(items[0], Value::BulkString(Some(b"0".to_vec())));
            assert_eq!(
                items[N - 1],
                Value::BulkString(Some((N - 1).to_string().into_bytes()))
            );
        }
        other => panic!("expected an array, got {other:?}"),
    }
}

#[test]
fn an_over_large_declared_count_is_still_refused_outright() {
    // The reservation cap is not a substitute for the element limit — a header
    // above `MAX_ARRAY_ELEMENTS` must still be rejected on the header alone.
    let err = Value::parse(b"*1000001\r\n").unwrap_err();
    assert!(err.to_string().contains("too large"), "got {err}");
}

// ── Streaming re-scan cost ────────────────────────────────────────────────────

/// A multi-bulk frame of `elements` bulk strings, each `value_len` bytes.
fn multibulk(elements: usize, value_len: usize) -> Vec<u8> {
    Value::Array(Some(
        (0..elements)
            .map(|i| Value::BulkString(Some(vec![b'a' + (i % 26) as u8; value_len])))
            .collect(),
    ))
    .serialize()
}

/// `Value::parse` restarts from the first byte on every call, so asking it
/// whether a frame had fully arrived rebuilt every element received so far —
/// one heap allocation per bulk string — and threw the lot away. Over a frame
/// spanning hundreds of TCP segments that is quadratic in allocations.
///
/// `Value::frame_len` answers the same question by walking headers and stepping
/// over payloads, so the streaming read path pays no allocation until the frame
/// is actually complete.
#[test]
fn measuring_a_partial_frame_allocates_nothing_worth_counting() {
    let frame = multibulk(4_000, 100);
    let partial = &frame[..frame.len() / 2];

    let scanning = bytes_allocated(|| {
        let r = Value::frame_len(std::hint::black_box(partial));
        assert!(r.is_err(), "half a frame should not measure as complete");
        std::hint::black_box(&r);
    });
    let building = bytes_allocated(|| {
        let r = Value::parse(std::hint::black_box(partial));
        assert!(r.is_err());
        std::hint::black_box(&r);
    });

    assert!(
        scanning < 4 * 1024,
        "frame_len allocated {scanning} bytes measuring a partial frame"
    );
    assert!(
        building > 20 * scanning.max(1),
        "parse allocated {building} bytes and frame_len {scanning} — the scan is \
         not meaningfully cheaper, so gating the read loop on it buys nothing"
    );
}

/// The whole point, end to end: feed a large frame segment by segment the way
/// TCP delivers it and total the allocations. Deciding completeness with
/// `frame_len` must cost a small fraction of deciding it with `parse`.
#[test]
fn streaming_a_large_frame_costs_far_less_when_gated_on_frame_len() {
    const SEGMENT: usize = 1400;
    let frame = multibulk(4_000, 100);

    let with_parse = bytes_allocated(|| {
        let mut buf: Vec<u8> = Vec::with_capacity(frame.len());
        for chunk in frame.chunks(SEGMENT) {
            buf.extend_from_slice(chunk);
            if Value::parse(std::hint::black_box(&buf)).is_ok() {
                break;
            }
        }
    });

    let with_frame_len = bytes_allocated(|| {
        let mut buf: Vec<u8> = Vec::with_capacity(frame.len());
        let mut need = 0usize;
        for chunk in frame.chunks(SEGMENT) {
            buf.extend_from_slice(chunk);
            if buf.len() < need {
                continue;
            }
            match Value::frame_len(std::hint::black_box(&buf)) {
                Ok(_) => {
                    let parsed = Value::parse(&buf);
                    assert!(parsed.is_ok());
                    std::hint::black_box(&parsed);
                    break;
                }
                Err(e) => {
                    assert!(e.is_incomplete(), "unexpected {e:?}");
                    need = e.needed();
                }
            }
        }
    });

    assert!(
        with_frame_len * 10 < with_parse,
        "gated streaming allocated {with_frame_len} bytes vs {with_parse} ungated — \
         expected at least a tenfold reduction"
    );
}

/// Two entry points into one parser is only safe if they agree. They share the
/// same code with construction switched off, and this pins that: for every
/// prefix of every frame, `frame_len` and `parse` must reach the same verdict
/// and the same length.
#[test]
fn frame_len_and_parse_agree_on_every_prefix() {
    let frames = [
        multibulk(12, 7),
        multibulk(1, 0),
        multibulk(3, 300),
        Value::Map(vec![
            (Value::BulkString(Some(b"a".to_vec())), Value::Integer(-1)),
            (Value::SimpleString("s".into()), Value::BulkString(None)),
        ])
        .serialize(),
        Value::Push(vec![
            Value::BulkString(Some(b"message".to_vec())),
            Value::Array(Some(vec![Value::Integer(7), Value::Array(None)])),
        ])
        .serialize(),
        b"*-1\r\n".to_vec(),
        b"+\r\n".to_vec(),
        b"$-1\r\n".to_vec(),
    ];

    for frame in &frames {
        for cut in 0..=frame.len() {
            let slice = &frame[..cut];
            match (Value::frame_len(slice), Value::parse(slice)) {
                (Ok(n), Ok((_, m))) => assert_eq!(n, m, "length disagreement at cut {cut}"),
                (Err(a), Err(b)) => assert_eq!(a, b, "error disagreement at cut {cut}"),
                (a, b) => panic!("verdict disagreement at cut {cut}: {a:?} vs {b:?}"),
            }
        }
    }

    // And on inputs that are not frames at all.
    for bad in [
        &b"!nope\r\n"[..],
        &b"*abc\r\n"[..],
        &b":x\r\n"[..],
        &b"$-7\r\n"[..],
        &b"%z\r\n"[..],
    ] {
        assert_eq!(
            Value::frame_len(bad).unwrap_err(),
            Value::parse(bad).unwrap_err(),
            "disagreement on {:?}",
            String::from_utf8_lossy(bad)
        );
    }
}
