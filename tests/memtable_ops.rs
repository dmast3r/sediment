//! M2 — In-memory memtable behavior tests.
//!
//! Where `api_surface.rs` pinned down the *shape* of the API (names,
//! signatures, error type), this file pins down its *behavior*: data put
//! in can be read back, overwrites win, deletes remove, and missing keys
//! return `None` rather than erroring.
//!
//! All storage is in-memory for M2 (a `HashMap` behind the scenes). The
//! `path` argument to `Db::open` is used only to create a directory; no
//! file contents are read or written yet.
//!
//! How to know M2 is done:
//!   1. Every test in this file PASSES (no `#[should_panic]` here — these
//!      assert real behavior).
//!   2. The corresponding tests in `api_surface.rs` have had their
//!      `#[should_panic]` removed and now pass for real (see that file).
//!   3. `./scripts/check.sh` is fully green.
//!
//! Each test uses a unique temp directory so runs don't collide. We build
//! the path from the test name; if you run tests in parallel (the default)
//! they won't step on each other.

use std::path::PathBuf;

use sediment::Db;

/// Build a unique temp directory path for a test. Not created here — we
/// want to prove `Db::open` creates it.
fn temp_dir(test_name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("sediment-m2-{test_name}"));
    // Best-effort clean slate: remove any leftovers from a previous run.
    let _ = std::fs::remove_dir_all(&p);
    p
}

// ---------------------------------------------------------------------------
// open
// ---------------------------------------------------------------------------

/// `Db::open` should create the data directory if it doesn't exist.
/// (In-memory storage, but the directory is the future home of SSTables/WAL.)
#[test]
fn open_creates_the_data_directory() {
    let dir = temp_dir("open-creates-dir");
    assert!(!dir.exists(), "precondition: dir should not exist yet");

    let _db = Db::open(&dir).expect("open should succeed");

    assert!(dir.exists(), "Db::open should have created the directory");
    assert!(dir.is_dir(), "the created path should be a directory");
}

/// Opening on the same directory twice — sequentially — should not error
/// (the directory already existing is fine; `create_dir_all` is idempotent).
/// The first handle is dropped before the second open: since M8's directory
/// lock, two *simultaneous* opens are forbidden by design, and that behavior
/// is asserted by `second_open_fails_while_first_is_alive` in db_startup.rs.
#[test]
fn open_is_idempotent_on_existing_directory() {
    let dir = temp_dir("open-idempotent");

    let db1 = Db::open(&dir).expect("first open should succeed");
    drop(db1); // release the directory lock
    let _db2 = Db::open(&dir).expect("reopen on existing dir should succeed");
}

// ---------------------------------------------------------------------------
// put / get round-trip
// ---------------------------------------------------------------------------

/// The fundamental contract: a value put under a key can be read back.
#[test]
fn put_then_get_returns_the_value() {
    let dir = temp_dir("put-get");
    let mut db = Db::open(&dir).unwrap();

    db.put(b"key", b"value").unwrap();

    let got = db.get(b"key").unwrap();
    assert_eq!(got, Some(b"value".to_vec()));
}

/// Getting a key that was never put returns `None` — NOT an error.
/// "Absent" is an expected outcome, modeled by `Option`, not `Result::Err`.
#[test]
fn get_on_missing_key_returns_none() {
    let dir = temp_dir("get-missing");
    let db = Db::open(&dir).unwrap();

    let got = db.get(b"never-inserted").unwrap();
    assert_eq!(got, None);
}

/// Multiple distinct keys coexist independently.
#[test]
fn multiple_keys_are_independent() {
    let dir = temp_dir("multi-key");
    let mut db = Db::open(&dir).unwrap();

    db.put(b"a", b"1").unwrap();
    db.put(b"b", b"2").unwrap();
    db.put(b"c", b"3").unwrap();

    assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(db.get(b"b").unwrap(), Some(b"2".to_vec()));
    assert_eq!(db.get(b"c").unwrap(), Some(b"3".to_vec()));
    assert_eq!(db.get(b"d").unwrap(), None);
}

// ---------------------------------------------------------------------------
// overwrite
// ---------------------------------------------------------------------------

/// Putting the same key twice overwrites: the latest value wins.
#[test]
fn put_same_key_overwrites() {
    let dir = temp_dir("overwrite");
    let mut db = Db::open(&dir).unwrap();

    db.put(b"key", b"first").unwrap();
    db.put(b"key", b"second").unwrap();

    assert_eq!(db.get(b"key").unwrap(), Some(b"second".to_vec()));
}

// ---------------------------------------------------------------------------
// delete
// ---------------------------------------------------------------------------

/// Deleting a key makes a subsequent get return `None`.
#[test]
fn delete_then_get_returns_none() {
    let dir = temp_dir("delete-get");
    let mut db = Db::open(&dir).unwrap();

    db.put(b"key", b"value").unwrap();
    db.delete(b"key").unwrap();

    assert_eq!(db.get(b"key").unwrap(), None);
}

/// Deleting a key that doesn't exist is NOT an error — it's a no-op.
#[test]
fn delete_missing_key_is_ok() {
    let dir = temp_dir("delete-missing");
    let mut db = Db::open(&dir).unwrap();

    // Should not panic or error.
    db.delete(b"never-existed").unwrap();

    assert_eq!(db.get(b"never-existed").unwrap(), None);
}

/// Delete then re-put: the key comes back to life with the new value.
#[test]
fn delete_then_put_revives_the_key() {
    let dir = temp_dir("delete-then-put");
    let mut db = Db::open(&dir).unwrap();

    db.put(b"key", b"v1").unwrap();
    db.delete(b"key").unwrap();
    assert_eq!(db.get(b"key").unwrap(), None);

    db.put(b"key", b"v2").unwrap();
    assert_eq!(db.get(b"key").unwrap(), Some(b"v2".to_vec()));
}

// ---------------------------------------------------------------------------
// byte-orientation (values are arbitrary bytes, not strings)
// ---------------------------------------------------------------------------

/// Keys and values are arbitrary bytes — including an empty value and
/// bytes that aren't valid UTF-8. This proves we store bytes, not strings.
#[test]
fn handles_arbitrary_bytes_including_empty_and_non_utf8() {
    let dir = temp_dir("arbitrary-bytes");
    let mut db = Db::open(&dir).unwrap();

    // Empty value is a legitimate value, distinct from "absent".
    db.put(b"empty-val", b"").unwrap();
    assert_eq!(db.get(b"empty-val").unwrap(), Some(Vec::new()));

    // Non-UTF-8 bytes round-trip fine (0xFF, 0xFE are invalid UTF-8 starts).
    let weird_key: &[u8] = &[0x00, 0xFF, 0xFE, 0x42];
    let weird_val: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];
    db.put(weird_key, weird_val).unwrap();
    assert_eq!(db.get(weird_key).unwrap(), Some(weird_val.to_vec()));
}
