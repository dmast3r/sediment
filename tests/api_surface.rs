//! Public API surface tests.
//!
//! These tests exercise the *shape* of sediment's public API — names,
//! method signatures, the error type, mutability discipline — rather than
//! deep behavior. Behavioral tests (round-trip, overwrite, delete
//! semantics) live in `memtable_ops.rs`.
//!
//! Originally written for M1 as `#[should_panic]` stubs against
//! `unimplemented!()`. Updated at M2: the methods now do real work, so
//! these assert the API is callable and returns the right *types*. The
//! explicit type annotations (`let _: Option<Vec<u8>>`) are the point —
//! they pin the signatures and fail to compile if the shape drifts.

use std::path::{Path, PathBuf};

use sediment::{Db, Error, Result};

/// Unique temp directory per test, cleaned of any prior-run leftovers.
fn temp_dir(test_name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("sediment-api-{test_name}"));
    let _ = std::fs::remove_dir_all(&p);
    p
}

// ---------------------------------------------------------------------------
// Constructor
// ---------------------------------------------------------------------------

/// `Db::open` should accept anything that can be referenced as a `Path`
/// (so callers can pass `&str`, `String`, `&Path`, `PathBuf`, etc. without
/// fuss) and return a `Result<Db>`.
///
/// Idiomatic Rust signature for this is:
///   `pub fn open<P: AsRef<Path>>(path: P) -> Result<Self>`
///
/// `AsRef<Path>` is the standard "I want any path-shaped thing" trait bound.
#[test]
fn open_returns_a_db() {
    let dir = temp_dir("open-returns-db");
    let _db: Db = Db::open(&dir).expect("open should succeed");
}

/// `Db::open` should also accept a `&Path` directly, proving the
/// `AsRef<Path>` bound (rather than e.g. `&str`-only).
#[test]
fn open_accepts_path_types() {
    let dir = temp_dir("open-path-types");
    let p: &Path = dir.as_path();
    let _db: Db = Db::open(p).expect("open should succeed for &Path too");
}

// ---------------------------------------------------------------------------
// Core operations — type shapes
// ---------------------------------------------------------------------------

/// `put` takes the key and value as byte slices (`&[u8]`) — bytes, not
/// strings, because the DB is format-agnostic. It returns `Result<()>`:
/// no payload on success, but I/O could fail.
#[test]
fn put_returns_unit_on_success() {
    let dir = temp_dir("put-unit");
    let mut db = Db::open(&dir).unwrap();
    let _: () = db.put(b"key", b"value").unwrap();
}

/// `get` takes a key as `&[u8]` and returns `Result<Option<Vec<u8>>>`.
///
/// Reasoning:
///   - `Result<...>` because I/O can fail.
///   - `Option<...>` because the key may simply not be present (that
///     is NOT an error — it's an expected outcome).
///   - `Vec<u8>` because we're handing back owned bytes the caller can
///     keep around. (See [[Slices and Owned Buffers]] in the KB.)
#[test]
fn get_returns_optional_owned_bytes() {
    let dir = temp_dir("get-optional");
    let db = Db::open(&dir).unwrap();
    let _: Option<Vec<u8>> = db.get(b"missing").unwrap();
}

/// `delete` takes a key and returns `Result<()>`. Deleting a key that
/// doesn't exist is NOT an error — it's a no-op (or, internally, writes
/// a tombstone; that's M3's concern).
#[test]
fn delete_returns_unit_on_success() {
    let dir = temp_dir("delete-unit");
    let mut db = Db::open(&dir).unwrap();
    let _: () = db.delete(b"key").unwrap();
}

// ---------------------------------------------------------------------------
// Mutability discipline
// ---------------------------------------------------------------------------

/// `get` is a *read* and should take `&self` (immutable borrow) so
/// multiple reads can coexist. This wouldn't compile if `get` required
/// `&mut self` while we only hold a shared binding.
#[test]
fn get_takes_immutable_borrow() {
    let dir = temp_dir("get-immutable");
    let db = Db::open(&dir).unwrap();
    let _ = db.get(b"a").unwrap();
    let _ = db.get(b"b").unwrap();
}

/// `put` and `delete` are *writes* and should take `&mut self`.
/// This test proves you can chain mutating calls through one binding.
#[test]
fn put_and_delete_take_mutable_borrow() {
    let dir = temp_dir("put-delete-mut");
    let mut db = Db::open(&dir).unwrap();
    db.put(b"k", b"v").unwrap();
    db.delete(b"k").unwrap();
}

// ---------------------------------------------------------------------------
// Error type shape
// ---------------------------------------------------------------------------

/// `sediment::Result<T>` should be a type alias for
/// `std::result::Result<T, sediment::Error>`. This proves the alias works
/// by storing the result of an operation in a `Result<T>` directly.
#[test]
fn result_alias_works() {
    let dir = temp_dir("result-alias");
    let mut db = Db::open(&dir).unwrap();
    let r: Result<()> = db.put(b"k", b"v");
    r.unwrap();
}

/// `sediment::Error` should implement `std::error::Error` (so it composes
/// with the `?` operator and the rest of the ecosystem) plus `Debug`,
/// `Display`, `Send`, and `Sync`. `thiserror`'s `#[derive(Error)]` gives
/// you all of these for free.
#[test]
fn error_type_implements_standard_traits() {
    fn assert_traits<E: std::error::Error + std::fmt::Debug + std::fmt::Display + Send + Sync>() {}
    assert_traits::<Error>();
}
