use crate::error::{Error, Result};
use crate::lookup::EntryState::{Absent, Live, Tombstone};
use crate::memtable::{Memtable, SkipListMemtable};
use crate::sstable::SsTable;
use std::path::{Path, PathBuf};

/// A database instance: insert key-value pairs, look up the value for a key, and delete keys.
/// Deleting a key that does not exist is a harmless no-op.
///
/// `Db` is generic over its in-memory memtable, defaulting to [`SkipListMemtable`]
/// so the common case is a plain `Db`. The backing structure is a *performance*
/// choice (a skip list keeps entries sorted, making the on-disk flush cheap and
/// behaving better under write concurrency), so callers who care can inject their
/// own via [`Db::open_with`]; everyone else uses [`Db::open`].
pub struct Db<M: Memtable = SkipListMemtable> {
    memtable: M,
    path: PathBuf,
    sstables: Vec<SsTable>,
}

impl Db<SkipListMemtable> {
    /// Open a database rooted at the given directory, backed by the default
    /// in-memory memtable, creating the directory (and any missing parents) if
    /// necessary. The directory will hold the write-ahead log and SSTables once
    /// on-disk persistence lands.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_with(SkipListMemtable::default(), path)
    }
}

impl<M: Memtable> Db<M> {
    /// Open a database backed by a caller-supplied memtable. This is the
    /// dependency-injection seam: `Db` imposes no requirement on *how* a
    /// memtable is constructed, so an implementation needing a capacity hint or
    /// allocator is free to build itself however it likes before handing it in.
    pub fn open_with<P: AsRef<Path>>(memtable: M, path: P) -> Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        std::fs::create_dir_all(&path_buf)?;
        Ok(Db {
            memtable,
            path: path_buf,
            sstables: Vec::new(),
        })
    }

    /// Insert a key-value pair, overwriting any existing value for the key.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        if u32::try_from(key.len()).is_err() {
            return Err(Error::KeyTooLong { len: key.len() });
        }

        if u32::try_from(value.len()).is_err() {
            return Err(Error::ValueTooLong { len: value.len() });
        }

        self.memtable.put(key, value);
        Ok(())
    }

    /// Look up the value for a key. Returns `Ok(Some(value))` if the key is present,
    /// or `Ok(None)` if it is absent (a missing key is not an error).
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        match self.memtable.lookup(key) {
            Live(v) => return Ok(Some(v)),
            Tombstone => return Ok(None),
            Absent => {}
        };

        for sstable in self.sstables.iter().rev() {
            match sstable.get(key)? {
                Live(v) => return Ok(Some(v)),
                Tombstone => return Ok(None),
                Absent => continue,
            }
        }

        Ok(None)
    }

    /// Delete a key. Deleting a key that does not exist is a harmless no-op.
    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.memtable.delete(key);
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let path = self.path.join("sstable.sst");
        SsTable::flush(&path, &self.memtable)?;
        self.sstables.push(SsTable::open(&path)?);
        self.memtable.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty directory for one test. Removed first so reruns start clean.
    fn temp_dir(test_name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("sediment-db-unit-{test_name}"));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    /// A successful flush empties the memtable: the entries now live in an
    /// SSTable, so keeping them in memory too is pure duplication.
    ///
    /// This is a unit test rather than an integration test because `Db::get`
    /// falls through to the SSTables, so a flushed key reads back identically
    /// whether the memtable was cleared. Nothing outside the crate can
    /// tell the difference; only code with access to the private `memtable`
    /// field can. A `flush` that forgot to `clear` would otherwise show up as
    /// unbounded memory growth, never as a wrong answer.
    #[test]
    fn flush_empties_the_memtable() {
        let dir = temp_dir("empties-memtable");
        let mut db = Db::open(&dir).expect("open");

        db.put(b"k", b"v").expect("put");
        assert!(
            matches!(db.memtable.lookup(b"k"), Live(_)),
            "precondition: the key is live in the memtable before the flush"
        );

        db.flush().expect("flush");

        assert!(
            matches!(db.memtable.lookup(b"k"), Absent),
            "flush must clear the memtable, leaving the key absent from memory"
        );
        assert_eq!(
            db.get(b"k").expect("get"),
            Some(b"v".to_vec()),
            "and the value must still be readable, now from the SSTable"
        );
    }
}
