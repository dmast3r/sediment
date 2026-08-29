use crate::lookup::EntryState;
use crate::memtable::Value::{Live, Tombstone};
use crate::memtable::{Memtable, Value};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct BTreeMapMemtable {
    map: BTreeMap<Vec<u8>, Value>,
}

impl Memtable for BTreeMapMemtable {
    fn put(&mut self, key: &[u8], val: &[u8]) {
        let val = val.to_vec();
        self.map.insert(key.to_vec(), Live(val));
    }

    fn delete(&mut self, key: &[u8]) {
        self.map.insert(key.to_vec(), Tombstone);
    }

    fn lookup(&self, key: &[u8]) -> EntryState {
        self.map
            .get(key)
            .map_or(EntryState::Absent, |entry| match entry {
                Live(val) => EntryState::Live(val.clone()),
                Tombstone => EntryState::Tombstone,
            })
    }

    fn iter(&self) -> impl Iterator<Item = (&[u8], Option<&[u8]>)> {
        self.map.iter().map(|(k, v)| {
            let val = match v {
                Live(bytes) => Some(bytes.as_slice()),
                Tombstone => None,
            };
            (k.as_slice(), val)
        })
    }

    fn clear(&mut self) {
        self.map.clear();
    }
}

// White-box tests of the tombstone model: they inspect `BTreeMapMemtable`'s
// internal `.map`, so they're tied to that representation (unlike the
// trait-level contract tests). The point they pin: delete tombstones in
// place rather than removing, so a deleted key stays tracked but reads absent.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::memtable::Memtable;
    use crate::memtable::btree::BTreeMapMemtable;
    use crate::memtable::contract::live_value;
    use rstest::{fixture, rstest};

    #[fixture]
    fn empty() -> BTreeMapMemtable {
        BTreeMapMemtable::default()
    }

    #[rstest]
    fn put_records_a_live_value(mut empty: BTreeMapMemtable) {
        empty.put(b"k", b"v");
        assert!(matches!(empty.map.get(b"k".as_slice()), Some(Live(_))));
        assert_eq!(live_value(&empty, b"k"), Some(b"v".to_vec()));
    }

    #[rstest]
    fn delete_writes_a_tombstone_rather_than_removing(mut empty: BTreeMapMemtable) {
        empty.put(b"k", b"v");
        empty.delete(b"k");

        assert_eq!(live_value(&empty, b"k"), None);
        // Key reads absent but is still tracked — as a tombstone, not removed.
        assert!(empty.map.contains_key(b"k".as_slice()));
        assert!(matches!(empty.map.get(b"k".as_slice()), Some(Tombstone)));
    }

    #[rstest]
    fn never_inserted_key_is_genuinely_absent(empty: BTreeMapMemtable) {
        // Reads absent like a tombstone, but with no entry tracked at all.
        assert_eq!(live_value(&empty, b"missing"), None);
        assert!(!empty.map.contains_key(b"missing".as_slice()));
    }

    #[rstest]
    fn delete_then_put_revives_as_live(mut empty: BTreeMapMemtable) {
        empty.put(b"k", b"v1");
        empty.delete(b"k");
        assert_eq!(live_value(&empty, b"k"), None);

        empty.put(b"k", b"v2");
        assert_eq!(live_value(&empty, b"k"), Some(b"v2".to_vec()));
        assert!(matches!(empty.map.get(b"k".as_slice()), Some(Live(_))));
    }

    #[rstest]
    fn delete_nonexistent_key_creates_a_tombstone(mut empty: BTreeMapMemtable) {
        // Deleting a never-seen key still records a tombstone: in a layered LSM
        // the key may live only in a lower layer, so the deletion must be
        // recorded regardless to shadow it.
        empty.delete(b"ghost");

        assert_eq!(live_value(&empty, b"ghost"), None);
        assert!(matches!(
            empty.map.get(b"ghost".as_slice()),
            Some(Tombstone)
        ));
    }

    #[rstest]
    fn double_delete_is_idempotent(mut empty: BTreeMapMemtable) {
        empty.put(b"k", b"v");
        empty.delete(b"k");
        empty.delete(b"k");

        assert_eq!(live_value(&empty, b"k"), None);
        assert!(matches!(empty.map.get(b"k".as_slice()), Some(Tombstone)));
    }
}
