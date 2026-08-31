//! `Db::flush` behavior, tested through the public API as an external consumer.
//!
//! These assert the *lifecycle* around a flush — where the file lands, what
//! happens to the memtable, what debris is left behind. The byte-level encoding
//! of the file itself is covered by the unit tests in `src/sstable.rs`.
//!
//! What these tests deliberately do NOT cover, so a green run is not mistaken
//! for durability coverage:
//!
//!   * That a failed write leaves no file at the final path. Reaching the
//!     failure branch needs the write to fail, which needs fault injection.
//!   * That a crash between `sync_all` and `rename` is recoverable.
//!   * That `sync_all` makes the data survive a power cut. Verifying that means
//!     actually cutting the power.
//!
//! All three need a way to make the filesystem fail on demand. See the note at
//! the bottom of this file for why that is not cheap here, and where it belongs.

use std::path::{Path, PathBuf};

use sediment::Db;

/// A fresh, empty directory for one test. Removed first so reruns start clean.
fn temp_dir(test_name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("sediment-db-flush-{test_name}"));
    let _ = std::fs::remove_dir_all(&p);
    p
}

/// Every file name directly inside `dir`, sorted. Directories are ignored.
fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("directory should exist")
        .map(|e| e.expect("readable entry"))
        .filter(|e| e.file_type().expect("file type").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// Where the file lands
// ---------------------------------------------------------------------------

/// The SSTable must be written inside the directory the `Db` was opened on.
///
/// Regression test: an earlier version wrote to a bare relative path, which the
/// OS resolves against the process's current working directory. The file landed
/// wherever `cargo test` happened to be running from, not in the database
/// directory.
#[test]
fn flush_writes_sstable_into_the_db_directory() {
    let dir = temp_dir("lands-in-db-dir");
    let mut db = Db::open(&dir).expect("open");

    db.put(b"k", b"v").expect("put");
    db.flush().expect("flush");

    let expected = dir.join("sstable.sst");
    assert!(
        expected.is_file(),
        "expected an SSTable at {expected:?}, found files: {:?}",
        file_names(&dir)
    );
    assert!(
        std::fs::metadata(&expected).expect("metadata").len() > 8,
        "file should hold at least one entry plus the 8-byte footer"
    );
}

/// Nothing is written outside the database directory. Checked by confirming the
/// directory holds exactly the one expected file — no stray siblings.
#[test]
fn flush_writes_exactly_one_file() {
    let dir = temp_dir("exactly-one-file");
    let mut db = Db::open(&dir).expect("open");

    db.put(b"a", b"1").expect("put");
    db.put(b"b", b"2").expect("put");
    db.flush().expect("flush");

    assert_eq!(
        file_names(&dir),
        vec!["sstable.sst".to_string()],
        "flush should leave exactly one file in the db directory"
    );
}

// ---------------------------------------------------------------------------
// No temp-file debris
// ---------------------------------------------------------------------------

/// The write path writes to `<name>.tmp` and then renames it to `<name>`, so a
/// crash mid-write cannot leave a partial file under the real name. After a
/// SUCCESSFUL flush the temp file must be gone — the rename consumed it.
///
/// A leftover `.tmp` would mean the rename did not happen, so the real file is
/// either missing or stale.
#[test]
fn successful_flush_leaves_no_tmp_file() {
    let dir = temp_dir("no-tmp-debris");
    let mut db = Db::open(&dir).expect("open");

    db.put(b"k", b"v").expect("put");
    db.flush().expect("flush");

    let names = file_names(&dir);
    let leftovers: Vec<&String> = names.iter().filter(|n| n.ends_with(".tmp")).collect();
    assert!(
        leftovers.is_empty(),
        "no .tmp files should remain after a successful flush, found: {leftovers:?}"
    );
    assert!(
        !dir.join("sstable.sst.tmp").exists(),
        "the specific temp path must not exist"
    );
}

// ---------------------------------------------------------------------------
// Memtable lifecycle
// ---------------------------------------------------------------------------

// "flush empties the memtable" is NOT tested here, and cannot be: as of M7
// `Db::get` falls through to the SSTables, so it returns the same value whether
// the key is in memory or on disk. That is the point of the read path, and it
// makes the memtable's emptiness invisible from outside the crate. The
// invariant is pinned by the unit test `flush_empties_the_memtable` in
// `src/db.rs`, which can see the private field.

/// The `Db` is still usable after a flush: writes land, reads see them.
///
/// This is the `Db`-level counterpart to the memtable `clear` contract. Emptying
/// the memtable must not damage it.
#[test]
fn db_is_usable_after_flush() {
    let dir = temp_dir("usable-after-flush");
    let mut db = Db::open(&dir).expect("open");

    db.put(b"before", b"1").expect("put");
    db.flush().expect("flush");

    db.put(b"after", b"2").expect("put");
    assert_eq!(db.get(b"after").expect("get"), Some(b"2".to_vec()));

    db.delete(b"after").expect("delete");
    assert_eq!(db.get(b"after").expect("get"), None);

    // And a second flush still succeeds.
    db.put(b"another", b"3").expect("put");
    db.flush().expect("second flush");
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

/// Flushing an empty memtable is harmless. It produces a valid, empty SSTable
/// (footer only) rather than erroring or skipping the write.
#[test]
fn flushing_an_empty_memtable_is_harmless() {
    let dir = temp_dir("empty-flush");
    let mut db = Db::open(&dir).expect("open");

    db.flush()
        .expect("flushing an empty memtable should succeed");

    let path = dir.join("sstable.sst");
    assert!(path.is_file(), "an empty flush still produces a file");
    assert_eq!(
        std::fs::metadata(&path).expect("metadata").len(),
        8,
        "an empty SSTable is just the 8-byte magic footer"
    );
    assert!(file_names(&dir).iter().all(|n| !n.ends_with(".tmp")));
}

/// Flushing twice in a row succeeds. The second flush overwrites the first
/// file, because the file name is fixed for this milestone.
///
/// This documents current behavior rather than endorsing it: the second flush
/// DESTROYING the first table is why sequenced file names are a deferred item.
/// A test that asserts the surviving file matches the second flush makes that
/// data loss visible instead of silent.
#[test]
fn second_flush_replaces_the_first_file() {
    let dir = temp_dir("double-flush");
    let mut db = Db::open(&dir).expect("open");

    db.put(b"first", b"1").expect("put");
    db.flush().expect("first flush");
    let after_first = std::fs::read(dir.join("sstable.sst")).expect("read");

    db.put(b"second", b"2").expect("put");
    db.flush().expect("second flush");
    let after_second = std::fs::read(dir.join("sstable.sst")).expect("read");

    assert_ne!(
        after_first, after_second,
        "the second flush must have rewritten the file"
    );
    assert!(
        after_second.windows(6).any(|w| w == b"second"),
        "the surviving file holds the second flush's key"
    );
    assert!(
        !after_second.windows(5).any(|w| w == b"first"),
        "the first flush's key is GONE — the fixed file name loses data. \
         Sequenced file names are a deferred item; this test pins the loss \
         so it is visible rather than silent."
    );
    assert_eq!(file_names(&dir), vec!["sstable.sst".to_string()]);
}

// ---------------------------------------------------------------------------
// Not tested here, and why
// ---------------------------------------------------------------------------
//
// Three properties of the write path have no test, and green above does not
// imply any of them hold:
//
//   1. A failed write leaves no file at the final path.
//   2. A crash between `sync_all` and `rename` is recoverable.
//   3. `sync_all` makes the bytes survive a power cut.
//
// All three need the filesystem to fail on demand. Why that is not cheap:
//
//   * A fake writer gets you partway. You can define a type that implements
//     `std::io::Write` and returns an error after N bytes, hand it to the
//     encoding loop, and prove the `?` operators propagate correctly. That is
//     worth doing and would be a genuine test of the encoder.
//   * It cannot reach the rest. `sync_all` is a method on `std::fs::File`, a
//     concrete type. `rename` is a free function in `std::fs`. Neither is
//     called through the `Write` trait, so substituting a fake writer does not
//     intercept them. Covering those needs an abstraction over the filesystem
//     itself — a trait with `create`, `sync`, and `rename` methods, with a real
//     implementation and a failing one. That is a real design change to the
//     write path, not a test-only addition.
//   * Property 3 is not testable in a unit test at all. Confirming data survives
//     a power cut requires cutting the power.
//
// Where this belongs: the proptest milestone already scopes "recovery from
// random crash points", which is the same machinery. Introducing a filesystem
// abstraction there covers 1 and 2 together. Property 3 stays untestable and
// is handled by review, not tests.
