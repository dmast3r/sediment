use crate::Error;
use crate::Error::{CorruptSsTable, Io};
use crate::error::Result;
use crate::lookup::EntryState;
use crate::memtable::Memtable;
use crate::record::{DecodeError, Record};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs::{File, rename};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::Bound::{Included, Unbounded};
use std::path::{Path, PathBuf};
// SSTable (Sorted String Table) — write and read path for on-disk storage.
//
// An SSTable is written exactly once from a flushed memtable and never
// modified. Its byte layout is:
//
// ```text
// [entry_0][entry_1]...[entry_N]   ← sorted by key, all entries including tombstones
// [footer: 8-byte magic]           ← marks a valid sediment SSTable
// ```
//
// Each entry:
// ```text
// [key_len: u32 LE][key bytes][tag: u8][value_len: u32 LE][value bytes]
// ```
// tag 0 = Live, tag 1 = Tombstone (value_len = 0, no value bytes).

pub struct SsTable {
    path: PathBuf,
    index: BTreeMap<Vec<u8>, u64>,
    footer_start: u64,
}

impl SsTable {
    const MAGIC: u64 = 0x5345_4449_4D45_4E54; // b"SEDIMENT" as u64 LE
    const FOOTER_LEN: u64 = 8;
    const BLOCK_SIZE: u64 = 4096;
    const TMP_EXTENSION: &str = "tmp";

    pub(crate) fn flush<P: AsRef<Path>, M: Memtable>(path: P, memtable: &M) -> Result<()> {
        let path_buf = Self::tmp_path(path.as_ref());

        let mut writer = BufWriter::new(File::create(&path_buf)?);

        memtable.iter().try_for_each(|(key, val)| -> Result<()> {
            Record::encode(&mut writer, key, val)
                .map_err(|e| Self::convert_decode_error(e, path.as_ref()))
        })?;

        writer.write_all(&Self::MAGIC.to_le_bytes())?;
        writer
            .into_inner()
            .map_err(|e| e.into_error())?
            .sync_all()?;

        rename(&path_buf, path.as_ref())?;

        Ok(())
    }

    pub(crate) fn is_tmp_path<P: AsRef<Path>>(path: P) -> bool {
        path.as_ref().extension().and_then(|s| s.to_str()) == Some(Self::TMP_EXTENSION)
    }

    pub(crate) fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();

        if file_len < Self::FOOTER_LEN {
            return Err(CorruptSsTable {
                path: path.to_path_buf(),
                detail: format!(
                    "file is {file_len} bytes, shorter than the {}-byte footer",
                    Self::FOOTER_LEN
                ),
            });
        }
        let footer_start = file_len - Self::FOOTER_LEN;

        file.seek(SeekFrom::Start(footer_start))?;
        let mut buffer = [0u8; Self::FOOTER_LEN as usize];
        file.read_exact(&mut buffer)?;
        let footer = u64::from_le_bytes(buffer);

        if footer != Self::MAGIC {
            return Err(CorruptSsTable {
                path: path.to_path_buf(),
                detail: format!(
                    "footer magic is {footer:#018x}, expected {:#018x}",
                    Self::MAGIC
                ),
            });
        }

        file.seek(SeekFrom::Start(0))?;
        let mut index: BTreeMap<Vec<u8>, u64> = BTreeMap::new();
        let mut total_bytes_read = 0;
        let mut bytes_counter = 0;

        let mut reader = BufReader::new(file);
        while total_bytes_read < footer_start {
            let record =
                Record::decode(&mut reader).map_err(|e| Self::convert_decode_error(e, path))?;
            let record_bytes = record.encoded_len();
            bytes_counter += record_bytes;

            if total_bytes_read == 0 || bytes_counter >= Self::BLOCK_SIZE {
                index.insert(record.key, total_bytes_read);
            }

            total_bytes_read += record_bytes;
            if bytes_counter >= Self::BLOCK_SIZE {
                bytes_counter = 0;
            }
        }

        if total_bytes_read != footer_start {
            return Err(CorruptSsTable {
                path: path.to_path_buf(),
                detail: format!("data region ends at {total_bytes_read}, expected {footer_start}"),
            });
        }

        Ok(SsTable {
            path: path.to_path_buf(),
            index,
            footer_start,
        })
    }

    pub(crate) fn get(&self, key: &[u8]) -> Result<EntryState> {
        let Some((_, &block_start)) = self
            .index
            .range::<[u8], _>((Unbounded, Included(key)))
            .next_back()
        else {
            return Ok(EntryState::Absent);
        };

        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(block_start))?;

        let mut offset = block_start;
        while offset < self.footer_start {
            let record =
                Record::decode(&mut file).map_err(|e| Self::convert_decode_error(e, &self.path))?;
            offset += record.encoded_len();
            match record.key.as_slice().cmp(key) {
                Ordering::Less => continue,
                Ordering::Equal => {
                    return Ok(record.val.map_or(EntryState::Tombstone, EntryState::Live));
                }
                Ordering::Greater => return Ok(EntryState::Absent),
            }
        }

        Ok(EntryState::Absent)
    }

    fn tmp_path<P: AsRef<Path>>(path: P) -> PathBuf {
        let mut tmp_path_string = path.as_ref().as_os_str().to_owned();
        tmp_path_string.push(".");
        tmp_path_string.push(Self::TMP_EXTENSION);
        PathBuf::from(tmp_path_string)
    }

    fn convert_decode_error(err: DecodeError, path: &Path) -> Error {
        match err {
            DecodeError::Incomplete { detail } => CorruptSsTable {
                path: path.to_path_buf(),
                detail,
            },
            DecodeError::Io(err) => Io(err),
            e @ DecodeError::InvalidTag { .. } => CorruptSsTable {
                path: path.to_path_buf(),
                detail: e.to_string(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// M6 tests — write path (written by Claude; implement against these).
//
// All tests below will NOT COMPILE until you:
//   1. add `flush_entries()` to the `Memtable` trait (in memtable.rs),
//   2. implement it for `SkipListMemtable`,
//   3. implement `SsTable::flush(path, memtable)` here.
//
// Entry encoding spec the tests assert against:
//   [key_len: u32 LE][key bytes][tag: u8][value_len: u32 LE][value bytes]
//   tag=0 Live, tag=1 Tombstone (value_len=0 for tombstones, uniform shape).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::memtable::Memtable;
    use crate::memtable::SkipListMemtable;

    /// Build a unique temp dir path for a test, cleaned of prior-run leftovers.
    fn temp_path(test_name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("sediment-m6-{test_name}.sst"));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// Read a `u32` little-endian from `bytes` at `pos`; return (value, new pos).
    fn read_u32_le(bytes: &[u8], pos: usize) -> (u32, usize) {
        let arr: [u8; 4] = bytes[pos..pos + 4].try_into().unwrap();
        (u32::from_le_bytes(arr), pos + 4)
    }

    // -----------------------------------------------------------------------
    // The magic number your footer must contain. Match this in your impl.
    // -----------------------------------------------------------------------
    const MAGIC: u64 = 0x5345_4449_4D45_4E54; // b"SEDIMENT" as u64 LE

    // -----------------------------------------------------------------------
    // 1. Footer is written and readable
    // -----------------------------------------------------------------------

    /// Flushing any memtable (even empty) must produce a file ending with the
    /// 8-byte magic number so a reader can verify file integrity.
    #[test]
    fn flush_writes_valid_footer() {
        let path = temp_path("footer");
        let mt = SkipListMemtable::default();

        super::SsTable::flush(&path, &mt).expect("flush should succeed");

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() >= 8, "file must be at least 8 bytes (footer)");

        let footer_start = bytes.len() - 8;
        let magic = u64::from_le_bytes(bytes[footer_start..].try_into().unwrap());
        assert_eq!(magic, MAGIC, "footer magic mismatch");
    }

    // -----------------------------------------------------------------------
    // 2. Empty memtable produces an empty data section
    // -----------------------------------------------------------------------

    #[test]
    fn flush_empty_memtable_produces_footer_only() {
        let path = temp_path("empty");
        let mt = SkipListMemtable::default();

        super::SsTable::flush(&path, &mt).expect("flush should succeed");

        let bytes = std::fs::read(&path).unwrap();
        // Only the 8-byte footer, no entries.
        assert_eq!(bytes.len(), 8, "empty flush should produce exactly 8 bytes");
    }

    // -----------------------------------------------------------------------
    // 3. Live entry encoding
    // -----------------------------------------------------------------------

    /// A flushed live entry must be encoded as:
    /// [key_len: u32 LE][key bytes][tag=0: u8][value_len: u32 LE][value bytes]
    #[test]
    fn live_entry_encoded_correctly() {
        let path = temp_path("live-entry");
        let mut mt = SkipListMemtable::default();
        mt.put(b"key", b"value");

        super::SsTable::flush(&path, &mt).expect("flush should succeed");

        let bytes = std::fs::read(&path).unwrap();

        let mut pos = 0;

        // key_len = 3
        let (key_len, new_pos) = read_u32_le(&bytes, pos);
        pos = new_pos;
        assert_eq!(key_len, 3, "key_len should be 3");

        // key bytes = b"key"
        assert_eq!(&bytes[pos..pos + 3], b"key");
        pos += 3;

        // tag = 0 (Live)
        assert_eq!(bytes[pos], 0, "tag should be 0 for a live entry");
        pos += 1;

        // value_len = 5
        let (value_len, new_pos) = read_u32_le(&bytes, pos);
        pos = new_pos;
        assert_eq!(value_len, 5, "value_len should be 5");

        // value bytes = b"value"
        assert_eq!(&bytes[pos..pos + 5], b"value");
        pos += 5;

        // footer
        assert_eq!(bytes.len() - pos, 8, "only footer should remain");
    }

    // -----------------------------------------------------------------------
    // 4. Tombstone encoding — uniform shape, NOT omitted
    // -----------------------------------------------------------------------

    /// A flushed tombstone must be encoded as:
    /// [key_len: u32 LE][key bytes][tag=1: u8][value_len=0: u32 LE]
    /// (zero value bytes — uniform shape; tombstones must NOT be omitted from
    /// the file or the deleted key could resurrect from a lower SSTable).
    #[test]
    fn tombstone_encoded_correctly_and_not_omitted() {
        let path = temp_path("tombstone");
        let mut mt = SkipListMemtable::default();
        mt.put(b"k", b"v");
        mt.delete(b"k"); // k is now a tombstone

        super::SsTable::flush(&path, &mt).expect("flush should succeed");

        let bytes = std::fs::read(&path).unwrap();

        let mut pos = 0;

        // key_len = 1
        let (key_len, new_pos) = read_u32_le(&bytes, pos);
        pos = new_pos;
        assert_eq!(key_len, 1);

        // key = b"k"
        assert_eq!(&bytes[pos..pos + 1], b"k");
        pos += 1;

        // tag = 1 (Tombstone)
        assert_eq!(bytes[pos], 1, "tag should be 1 for a tombstone");
        pos += 1;

        // value_len = 0
        let (value_len, new_pos) = read_u32_le(&bytes, pos);
        pos = new_pos;
        assert_eq!(value_len, 0, "tombstone value_len must be 0");

        // footer only remains
        assert_eq!(
            bytes.len() - pos,
            8,
            "only footer should remain after tombstone"
        );
    }

    // -----------------------------------------------------------------------
    // 5. Entries are sorted by key
    // -----------------------------------------------------------------------

    /// Entries must appear in ascending key order (lexicographic byte order)
    /// regardless of insertion order into the memtable.
    #[test]
    fn entries_are_sorted_by_key() {
        let path = temp_path("sorted");
        let mut mt = SkipListMemtable::default();
        // Insert deliberately out of order.
        mt.put(b"cherry", b"3");
        mt.put(b"apple", b"1");
        mt.put(b"banana", b"2");

        super::SsTable::flush(&path, &mt).expect("flush should succeed");

        let bytes = std::fs::read(&path).unwrap();

        // Extract just the key sequence from the file.
        let mut keys: Vec<Vec<u8>> = Vec::new();
        let mut pos = 0;
        let data_end = bytes.len() - 8; // exclude footer

        while pos < data_end {
            let (key_len, new_pos) = read_u32_le(&bytes, pos);
            pos = new_pos;
            let key = bytes[pos..pos + key_len as usize].to_vec();
            pos += key_len as usize;
            let tag = bytes[pos];
            pos += 1;
            let (value_len, new_pos) = read_u32_le(&bytes, pos);
            pos = new_pos;
            pos += value_len as usize;
            keys.push(key);
            let _ = tag; // suppress unused warning; not asserted here
        }

        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "entries must be in ascending key order");
    }

    // -----------------------------------------------------------------------
    // 6. Mixed live + tombstone: correct entry count and tombstone not omitted
    // -----------------------------------------------------------------------

    /// Flushing a mix of live and tombstoned keys must emit an entry for every
    /// key — tombstones included. This is the invariant that prevents deleted
    /// keys from resurrecting when a lower-layer SSTable still holds them.
    #[test]
    fn mixed_live_and_tombstone_all_entries_written() {
        let path = temp_path("mixed");
        let mut mt = SkipListMemtable::default();
        mt.put(b"a", b"1");
        mt.put(b"b", b"2");
        mt.delete(b"a"); // a is now a tombstone
        mt.put(b"c", b"3");
        // Expected on disk: a(tombstone), b(live), c(live) — 3 entries.

        super::SsTable::flush(&path, &mt).expect("flush should succeed");

        let bytes = std::fs::read(&path).unwrap();
        let mut entry_count = 0;
        let mut tombstone_count = 0;
        let mut pos = 0;
        let data_end = bytes.len() - 8;

        while pos < data_end {
            let (key_len, new_pos) = read_u32_le(&bytes, pos);
            pos = new_pos;
            pos += key_len as usize;
            let tag = bytes[pos];
            pos += 1;
            let (value_len, new_pos) = read_u32_le(&bytes, pos);
            pos = new_pos;
            pos += value_len as usize;

            entry_count += 1;
            if tag == 1 {
                tombstone_count += 1;
            }
        }

        assert_eq!(
            entry_count, 3,
            "all 3 entries (live + tombstone) must be written"
        );
        assert_eq!(tombstone_count, 1, "exactly one tombstone");
    }

    // -----------------------------------------------------------------------
    // 7. Arbitrary bytes (including empty value and non-UTF-8) round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn arbitrary_bytes_encoded_correctly() {
        let path = temp_path("arbitrary");
        let mut mt = SkipListMemtable::default();

        let weird_k: &[u8] = &[0x00, 0xFF, 0xFE];
        let weird_v: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];
        mt.put(weird_k, weird_v);
        mt.put(b"empty-val", b"");

        super::SsTable::flush(&path, &mt).expect("flush should succeed");

        let file_bytes = std::fs::read(&path).unwrap();
        let data_end = file_bytes.len() - 8;
        let data = &file_bytes[..data_end];

        // Decode all entries and collect (key, tag, value).
        let mut entries: Vec<(Vec<u8>, u8, Vec<u8>)> = Vec::new();
        let mut pos = 0;
        while pos < data.len() {
            let (kl, p) = read_u32_le(data, pos);
            pos = p;
            let k = data[pos..pos + kl as usize].to_vec();
            pos += kl as usize;
            let tag = data[pos];
            pos += 1;
            let (vl, p) = read_u32_le(data, pos);
            pos = p;
            let v = data[pos..pos + vl as usize].to_vec();
            pos += vl as usize;
            entries.push((k, tag, v));
        }

        // Entries are sorted: [0x00,0xFF,0xFE] < b"empty-val"
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, weird_k);
        assert_eq!(entries[0].1, 0); // Live
        assert_eq!(entries[0].2, weird_v);
        assert_eq!(entries[1].0, b"empty-val");
        assert_eq!(entries[1].1, 0); // Live
        assert_eq!(entries[1].2, b""); // empty value, but tag=0 not 1
    }

    // -----------------------------------------------------------------------
    // M8 step 0b — mid-record EOF is corruption, not an I/O error.
    //
    // Decided in the 2026-08-29 error-handling design session (see
    // docs/design/error-handling-boundary.md, "Errors must distinguish what
    // callers must treat differently"). Inside a scan whose bounds guarantee
    // a complete record, running out of bytes means the file lies about its
    // own structure: that is CorruptSsTable, so the caller knows retrying is
    // useless. Every other io::ErrorKind must still pass through as Io.
    // -----------------------------------------------------------------------

    /// A file with a valid footer whose one record claims a value longer than
    /// the bytes that exist. `open`'s scan hits EOF mid-record. The caller
    /// must see "this file is corrupt", not "an I/O operation failed".
    #[test]
    fn truncated_mid_record_is_corruption_not_io() {
        let path = temp_path("mid-record-eof");

        // Hand-craft the file: one record whose val_len lies.
        //   key_len=5, key="hello", tag=0, val_len=1000, but only 10 value
        //   bytes present — then a valid magic footer, so the footer check
        //   passes and the scan is what discovers the problem.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&5u32.to_le_bytes());
        bytes.extend_from_slice(b"hello");
        bytes.push(0); // tag: Live
        bytes.extend_from_slice(&1000u32.to_le_bytes()); // claims 1000 bytes...
        bytes.extend_from_slice(&[0xAB; 10]); // ...but only 10 exist
        bytes.extend_from_slice(&MAGIC.to_le_bytes());
        std::fs::write(&path, &bytes).expect("write crafted file");

        let Err(err) = super::SsTable::open(&path) else {
            panic!("open must fail on a file whose record overruns the data");
        };

        assert!(
            matches!(err, crate::Error::CorruptSsTable { .. }),
            "running out of bytes mid-record is corruption evidence and must \
             surface as CorruptSsTable so the caller does not blindly retry; \
             got instead: {err:?}"
        );
    }
}