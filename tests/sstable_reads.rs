//! M7 — SSTable read path, tested through `Db`'s public API.
//!
//! At this milestone `Db::get` consults both the in-memory memtable and the
//! on-disk SSTable (if one exists). The tests below exercise that combined
//! read path.
//!
//! What these tests do NOT cover, and why:
//!   - Concurrent reads/writes: still single-threaded (M12/M13).
//!   - Multiple SSTables: one SSTable exists after one flush (M10).
//!   - Compaction: not implemented (M12).
//!
//! The tombstone rule that appears in several tests is the load-bearing
//! invariant: a tombstone anywhere in the search path is a stopping hit.
//! At M7 there is only one SSTable so falling through is impossible, but the
//! tests pin the correct behavior now so M10 does not accidentally break it.

use std::path::PathBuf;

use sediment::Db;

fn temp_dir(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("sediment-m7-{name}"));
    let _ = std::fs::remove_dir_all(&p);
    p
}

// ---------------------------------------------------------------------------
// Core contract
// ---------------------------------------------------------------------------

/// The simplest possible read path: write a key, flush, read it back.
///
/// Before M7 `get` returns `None` after a flush because the memtable is
/// cleared and nothing reads SSTables. This is the test that turns green
/// when the SSTable read path lands.
#[test]
fn get_returns_flushed_value() {
    let dir = temp_dir("get-flushed");
    let mut db = Db::open(&dir).expect("open");

    db.put(b"key", b"value").expect("put");
    db.flush().expect("flush");

    assert_eq!(
        db.get(b"key").expect("get"),
        Some(b"value".to_vec()),
        "a flushed key must be readable from the SSTable"
    );
}

/// Multiple keys survive a flush and are independently readable.
#[test]
fn multiple_keys_survive_flush() {
    let dir = temp_dir("multi-key-flush");
    let mut db = Db::open(&dir).expect("open");

    db.put(b"apple", b"1").expect("put");
    db.put(b"banana", b"2").expect("put");
    db.put(b"cherry", b"3").expect("put");
    db.flush().expect("flush");

    assert_eq!(db.get(b"apple").expect("get"), Some(b"1".to_vec()));
    assert_eq!(db.get(b"banana").expect("get"), Some(b"2".to_vec()));
    assert_eq!(db.get(b"cherry").expect("get"), Some(b"3".to_vec()));
    assert_eq!(db.get(b"date").expect("get"), None, "absent key still None");
}

/// The memtable is checked before the SSTable.
///
/// After a flush, a new write goes into the memtable. That value is newer
/// than whatever is in the SSTable. `get` must return the memtable value,
/// not the flushed one.
#[test]
fn memtable_shadows_sstable() {
    let dir = temp_dir("memtable-shadows");
    let mut db = Db::open(&dir).expect("open");

    db.put(b"k", b"old").expect("put");
    db.flush().expect("flush");

    // Write a newer value — now only in the memtable.
    db.put(b"k", b"new").expect("put");

    assert_eq!(
        db.get(b"k").expect("get"),
        Some(b"new".to_vec()),
        "the memtable value must shadow the flushed value"
    );
}

// ---------------------------------------------------------------------------
// Tombstone rule
// ---------------------------------------------------------------------------

/// A tombstone in the SSTable means the key was deleted before the flush.
/// `get` must return `None`, not fall through to some older value.
///
/// At M7 there is no older layer to fall through to, but pinning this now
/// ensures M10 does not accidentally resurrect deleted keys.
#[test]
fn tombstone_in_sstable_returns_none() {
    let dir = temp_dir("tombstone-sstable");
    let mut db = Db::open(&dir).expect("open");

    db.put(b"k", b"v").expect("put");
    db.delete(b"k").expect("delete"); // tombstone in memtable
    db.flush().expect("flush"); // tombstone now in SSTable

    assert_eq!(
        db.get(b"k").expect("get"),
        None,
        "a tombstone in the SSTable means the key is absent"
    );
}

/// A tombstone in the memtable must stop the search even if the SSTable
/// holds a live value for the same key.
///
/// Sequence: put, flush (live value in SSTable), delete (tombstone in memtable).
/// `get` must return `None` — the memtable tombstone is newer.
#[test]
fn memtable_tombstone_shadows_sstable_live_value() {
    let dir = temp_dir("memtable-tombstone-shadows");
    let mut db = Db::open(&dir).expect("open");

    db.put(b"k", b"v").expect("put");
    db.flush().expect("flush"); // live value now in SSTable

    db.delete(b"k").expect("delete"); // tombstone now in memtable

    assert_eq!(
        db.get(b"k").expect("get"),
        None,
        "a memtable tombstone must shadow the flushed live value"
    );
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

/// A key absent from both the memtable and the SSTable returns `None`.
#[test]
fn absent_key_returns_none_after_flush() {
    let dir = temp_dir("absent-after-flush");
    let mut db = Db::open(&dir).expect("open");

    db.put(b"k", b"v").expect("put");
    db.flush().expect("flush");

    assert_eq!(db.get(b"never").expect("get"), None);
}

/// `get` works correctly before any flush has happened — no SSTable exists yet.
#[test]
fn get_works_before_any_flush() {
    let dir = temp_dir("no-flush-yet");
    let mut db = Db::open(&dir).expect("open");

    assert_eq!(db.get(b"k").expect("get"), None, "absent key before flush");

    db.put(b"k", b"v").expect("put");
    assert_eq!(
        db.get(b"k").expect("get"),
        Some(b"v".to_vec()),
        "memtable read before flush"
    );
}

/// A write after a flush is readable from the memtable even if not yet flushed.
/// This is a basic consistency check, not a durability claim.
#[test]
fn post_flush_write_is_readable() {
    let dir = temp_dir("post-flush-write");
    let mut db = Db::open(&dir).expect("open");

    db.put(b"a", b"1").expect("put");
    db.flush().expect("flush");

    // New write not yet flushed.
    db.put(b"b", b"2").expect("put");

    assert_eq!(
        db.get(b"a").expect("get"),
        Some(b"1".to_vec()),
        "from SSTable"
    );
    assert_eq!(
        db.get(b"b").expect("get"),
        Some(b"2".to_vec()),
        "from memtable"
    );
}

/// A key overwritten after a flush: the new value is in the memtable,
/// the old value is in the SSTable. `get` returns the newer one.
#[test]
fn overwrite_after_flush() {
    let dir = temp_dir("overwrite-after-flush");
    let mut db = Db::open(&dir).expect("open");

    db.put(b"k", b"old").expect("put");
    db.flush().expect("flush");
    db.put(b"k", b"new").expect("put");

    assert_eq!(
        db.get(b"k").expect("get"),
        Some(b"new".to_vec()),
        "newer memtable value must win over flushed value"
    );
}
