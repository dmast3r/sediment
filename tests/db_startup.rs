//! M8 integration tests — startup, durability, and the write-ahead log.
//!
//! These tests are the specification for M8 (see docs/milestones/M8.md and
//! docs/design/error-handling-boundary.md). They are written BEFORE the
//! implementation and fail until it exists. Each doc comment names the policy
//! the test pins.
//!
//! Provisional spec pinned here (rename HERE FIRST if you disagree, then
//! implement to match):
//!   - The WAL lives at `<db dir>/wal`.
//!   - A WAL record reuses the SSTable entry encoding:
//!     [key_len: u32 LE][key][tag: u8][val_len: u32 LE][val],
//!     tag 0 = live, tag 1 = tombstone (val_len 0).
//!
//! Deliberately NOT here yet:
//!   - "two flushes produce two files" — waits on the manifest design session
//!     (M8 step 6).
//!   - oversized-key validation (step 0a) — the test design needs discussion
//!     first (a 4 GiB key cannot be cheaply allocated in a unit test).

use sediment::{Db, Error};
use std::path::PathBuf;

/// Fresh directory per test, cleaned of prior-run leftovers.
fn temp_dir(test_name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("sediment-m8-{test_name}"));
    let _ = std::fs::remove_dir_all(&p);
    p
}

/// The WAL's location inside a db directory. Provisional spec.
fn wal_path(dir: &std::path::Path) -> PathBuf {
    dir.join("wal")
}

/// Test-side encoder for one WAL record (mirrors the SSTable entry encoding).
/// `None` value = tombstone.
fn encode_record(key: &[u8], val: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&u32::try_from(key.len()).unwrap().to_le_bytes());
    out.extend_from_slice(key);
    match val {
        Some(v) => {
            out.push(0);
            out.extend_from_slice(&u32::try_from(v.len()).unwrap().to_le_bytes());
            out.extend_from_slice(v);
        }
        None => {
            out.push(1);
            out.extend_from_slice(&0u32.to_le_bytes());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The acknowledgment boundary: Ok from put/delete means "recoverable".
// ---------------------------------------------------------------------------

/// An acknowledged `put` survives losing the process before any flush.
///
/// `drop` here simulates the *data-loss* aspect of a crash: the memtable is
/// gone and nothing was flushed, so the value can only come back via WAL
/// replay at reopen. (The *torn-write* aspect of a crash is simulated by the
/// crafted-file tests below, which drop can't produce.)
#[test]
fn put_survives_reopen_without_flush() {
    let dir = temp_dir("put-survives");
    let mut db = Db::open(&dir).expect("first open");
    db.put(b"k", b"v").expect("put");
    drop(db); // no flush — the WAL is the only durable copy

    let db = Db::open(&dir).expect("reopen");
    assert_eq!(
        db.get(b"k").expect("get"),
        Some(b"v".to_vec()),
        "an acknowledged put must be recovered from the WAL at reopen"
    );
}

/// An acknowledged `delete` survives the same round trip: the tombstone must
/// be logged like any write, or a deleted key resurrects after a crash.
///
/// The second key is a witness: it proves replay actually ran, so the deleted
/// key's absence means "tombstone recovered" and not "nothing recovered".
/// Without it this test passes vacuously against an empty database.
#[test]
fn delete_survives_reopen_without_flush() {
    let dir = temp_dir("delete-survives");
    let mut db = Db::open(&dir).expect("first open");
    db.put(b"k", b"v").expect("put");
    db.put(b"witness", b"w").expect("put witness");
    db.delete(b"k").expect("delete");
    drop(db);

    let db = Db::open(&dir).expect("reopen");
    assert_eq!(
        db.get(b"witness").expect("get"),
        Some(b"w".to_vec()),
        "witness key must be recovered — otherwise replay never ran and the \
         deleted key's absence below proves nothing"
    );
    assert_eq!(
        db.get(b"k").expect("get"),
        None,
        "a logged tombstone must shadow the logged put after replay"
    );
}

/// Recovery must be repeatable: replay rebuilds the memtable, which is
/// volatile, so the log must survive the open that consumed it. A recovery
/// that resets the log at open destroys the only durable copy of acknowledged
/// writes — data the NEXT crash then loses, invisibly to any single-reopen
/// test (the value still reads back from the in-process memtable).
///
/// Regression guard: this exact bug existed mid-M8 (`reset` called in
/// `open_with` instead of `flush`) while the single-reopen tests above
/// stayed green.
#[test]
fn recovery_survives_a_second_reopen_without_flush() {
    let dir = temp_dir("second-reopen");
    let mut db = Db::open(&dir).expect("first open");
    db.put(b"k", b"v").expect("put");
    drop(db);

    // First recovery: replay populates the memtable. If this open reset the
    // log, the acknowledged put now lives only in volatile memory.
    let db = Db::open(&dir).expect("second open");
    drop(db); // crash again, still before any flush

    let db = Db::open(&dir).expect("third open");
    assert_eq!(
        db.get(b"k").expect("get"),
        Some(b"v".to_vec()),
        "an acknowledged put must survive every crash until it is flushed; \
         recovery must not consume the log it replays"
    );
}

// ---------------------------------------------------------------------------
// WAL recovery semantics (docs/design/error-handling-boundary.md,
// "The acknowledgment boundary").
// ---------------------------------------------------------------------------

/// A half-written final record is the expected debris of a crash mid-append.
/// Replay recovers every complete record, truncates the torn tail silently,
/// and open succeeds — no error: that record's `put` never returned `Ok`.
#[test]
fn torn_tail_is_truncated_and_open_succeeds() {
    let dir = temp_dir("torn-tail");
    std::fs::create_dir_all(&dir).expect("create dir");

    let rec_a = encode_record(b"alpha", Some(b"1"));
    let rec_b = encode_record(b"beta", Some(b"2"));
    let rec_c = encode_record(b"gamma", Some(b"3"));

    let mut wal = Vec::new();
    wal.extend_from_slice(&rec_a);
    wal.extend_from_slice(&rec_b);
    wal.extend_from_slice(&rec_c[..rec_c.len() / 2]); // the torn tail
    std::fs::write(wal_path(&dir), &wal).expect("write crafted wal");

    let db = Db::open(&dir).expect("open must tolerate a torn tail");
    assert_eq!(db.get(b"alpha").expect("get"), Some(b"1".to_vec()));
    assert_eq!(db.get(b"beta").expect("get"), Some(b"2".to_vec()));
    assert_eq!(
        db.get(b"gamma").expect("get"),
        None,
        "the torn record was never acknowledged; it must not be recovered"
    );

    let len = std::fs::metadata(wal_path(&dir)).expect("stat wal").len();
    assert_eq!(
        len,
        (rec_a.len() + rec_b.len()) as u64,
        "recovery must truncate the torn tail off the log"
    );
}

/// Damage BEFORE the last complete record is different: sequential appends
/// mean the damaged region once held fully-written, acknowledged records.
/// Open must fail with a corruption-shaped error — never silently skip.
///
/// The damage used here is an invalid tag byte mid-log (framing survives, so
/// the reader can see bytes continue after the bad record — provably not a
/// torn tail). Note the accepted M8 gap: mid-log damage that mimics a torn
/// tail (e.g. a corrupted length field consuming the rest of the file) is
/// undetectable without per-record CRCs, which arrive at M11.
#[test]
fn mid_log_damage_fails_open_as_corruption() {
    let dir = temp_dir("mid-log-damage");
    std::fs::create_dir_all(&dir).expect("create dir");

    let rec_a = encode_record(b"alpha", Some(b"1"));
    let mut rec_bad = encode_record(b"beta", Some(b"2"));
    rec_bad[4 + 4] = 7; // key_len(4) + "beta"(4) → the tag byte: 7 is no tag
    let rec_c = encode_record(b"gamma", Some(b"3"));

    let mut wal = Vec::new();
    wal.extend_from_slice(&rec_a);
    wal.extend_from_slice(&rec_bad);
    wal.extend_from_slice(&rec_c);
    std::fs::write(wal_path(&dir), &wal).expect("write crafted wal");

    let Err(err) = Db::open(&dir) else {
        panic!("open must fail: acknowledged data is damaged mid-log");
    };
    // TODO(tighten): assert the exact corruption variant once you name it
    // (M8.md leaves the CorruptWal-shaped variant's name to you). The half
    // of the spec that is already settled: it must NOT classify as Io —
    // environmental errors mean "retry"; this means "recover".
    assert!(
        !matches!(err, Error::Io(_)),
        "mid-log damage is corruption, not an environmental I/O failure; got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Startup obstacles (M8 steps 1 and 2).
// ---------------------------------------------------------------------------

/// Two processes must not share a directory. The second open fails fast with
/// a lock error while the first Db lives, and succeeds after it is dropped.
#[test]
fn second_open_fails_while_first_is_alive() {
    let dir = temp_dir("second-open");
    let db = Db::open(&dir).expect("first open");

    let Err(err) = Db::open(&dir) else {
        panic!("second open on a locked directory must fail");
    };
    assert!(
        matches!(err, Error::DatabaseDirectoryAlreadyInUse { .. }),
        "a locked directory must surface as its own variant so the caller \
         knows to wait or exit, not retry blindly; got: {err:?}"
    );

    drop(db);
    Db::open(&dir).expect("open must succeed once the lock holder is gone");
}

/// A stale `.tmp` file is debris from a dead process's interrupted flush.
/// Under the lock, no flush can be in flight at open, so open sweeps it.
#[test]
fn stale_tmp_is_swept_at_open() {
    let dir = temp_dir("tmp-sweep");
    std::fs::create_dir_all(&dir).expect("create dir");
    let tmp = dir.join("sstable.sst.tmp");
    std::fs::write(&tmp, b"debris from a dead process").expect("write tmp");

    let _db = Db::open(&dir).expect("open");
    assert!(
        !tmp.exists(),
        "open must sweep stale .tmp files; a crashed flush's debris survived"
    );
}

// ---------------------------------------------------------------------------
// WAL lifecycle (M8 step 5).
// ---------------------------------------------------------------------------

/// After a successful flush the flushed records are durable in the SSTable,
/// so the WAL's job for them is done: it must be reset to empty. Without the
/// reset, every reopen replays writes that already live in SSTables, and the
/// log grows without bound.
///
/// Only the end state is pinned here. The crash-window ordering (truncate
/// strictly after rename + directory fsync) cannot be observed from an
/// integration test and belongs to M9's fault injection.
#[test]
fn wal_is_reset_after_successful_flush() {
    let dir = temp_dir("wal-reset");
    let mut db = Db::open(&dir).expect("open");
    db.put(b"k", b"v").expect("put");
    db.flush().expect("flush");

    let len = std::fs::metadata(wal_path(&dir))
        .expect("the WAL file must exist after open+put")
        .len();
    assert_eq!(
        len, 0,
        "a successful flush must reset the WAL; its records are now durable \
         in the SSTable and would otherwise be replayed forever"
    );
}
