// BTreeMap-backed memtable is a test-only differential oracle for the skip
// list — the production default is `SkipListMemtable`, so it is never compiled
// into release builds.
#[cfg(test)]
pub mod btree;
pub mod skiplist;

use crate::lookup::EntryState;
pub(crate) use skiplist::SkipListMemtable;

pub trait Memtable {
    fn put(&mut self, key: &[u8], val: &[u8]);
    fn delete(&mut self, key: &[u8]);
    fn lookup(&self, key: &[u8]) -> EntryState;
    /// Every entry in ascending key order, tombstones included.
    ///
    /// `Some(bytes)` is a live value; `None` is a tombstone. Yields borrowed
    /// slices, so the returned iterator borrows `self` — nothing can mutate the
    /// memtable while it is alive, which the compiler enforces.
    ///
    /// Tombstone handling is the caller's decision. For live entries only:
    /// `iter().filter_map(|(k, v)| v.map(|v| (k, v)))`
    fn iter(&self) -> impl Iterator<Item = (&[u8], Option<&[u8]>)>;
    fn clear(&mut self);
}

enum Value {
    Live(Vec<u8>),
    Tombstone,
}

// Behavioral contract every `Memtable` impl must satisfy, written once and run
// against any implementation (unlike the white-box tests above, these touch
// only the trait's public surface). Kept `pub(crate)` so each impl's own test
// module can run them.
#[cfg(test)]
mod contract {
    use crate::lookup::EntryState;
    use crate::memtable::Memtable;
    use crate::memtable::btree::BTreeMapMemtable;

    /// Live entries only, owned, in key order — the shape most assertions want.
    ///
    /// `iter()` yields every entry including tombstones, because filtering is
    /// the caller's decision. Several tests want the live-only view, so the
    /// filter lives here once instead of being repeated at each call site.
    fn live_entries<M: Memtable>(mt: &M) -> Vec<(Vec<u8>, Vec<u8>)> {
        mt.iter()
            .filter_map(|(k, v)| v.map(|v| (k.to_vec(), v.to_vec())))
            .collect()
    }

    /// The live value for a key, if any — the shape most assertions want.
    ///
    /// `lookup()` reports three states because `Db::get` must distinguish a
    /// tombstone (stop searching) from an absent key (keep searching older
    /// SSTables). Most assertions only care whether a live value came back, so
    /// that collapse lives here rather than on the trait: "give me just the live
    /// value" is what a test wants to check, not something the storage layer
    /// needs to do.
    pub(crate) fn live_value<M: Memtable>(mt: &M, key: &[u8]) -> Option<Vec<u8>> {
        match mt.lookup(key) {
            EntryState::Live(v) => Some(v),
            EntryState::Tombstone | EntryState::Absent => None,
        }
    }

    /// Every entry including tombstones, owned, in key order.
    fn all_entries<M: Memtable>(mt: &M) -> Vec<(Vec<u8>, Option<Vec<u8>>)> {
        mt.iter()
            .map(|(k, v)| (k.to_vec(), v.map(|v| v.to_vec())))
            .collect()
    }

    pub(crate) fn memtable_contract<M: Memtable + Default>() {
        let mut mt = M::default();

        assert_eq!(live_value(&mt, b"nope"), None, "missing key should be None");

        mt.put(b"k", b"v");
        assert_eq!(
            live_value(&mt, b"k"),
            Some(b"v".to_vec()),
            "put/get round-trip"
        );

        mt.put(b"k", b"v2");
        assert_eq!(
            live_value(&mt, b"k"),
            Some(b"v2".to_vec()),
            "overwrite wins"
        );

        mt.put(b"a", b"1");
        mt.put(b"b", b"2");
        assert_eq!(live_value(&mt, b"a"), Some(b"1".to_vec()));
        assert_eq!(live_value(&mt, b"b"), Some(b"2".to_vec()));

        mt.delete(b"k");
        assert_eq!(live_value(&mt, b"k"), None, "deleted key reads as absent");

        mt.delete(b"ghost");
        assert_eq!(
            live_value(&mt, b"ghost"),
            None,
            "delete of never-seen key is a no-op"
        );

        mt.put(b"k", b"v3");
        assert_eq!(
            live_value(&mt, b"k"),
            Some(b"v3".to_vec()),
            "re-put after delete"
        );

        // Values are opaque bytes: empty and non-UTF-8 must round-trip.
        mt.put(b"empty", b"");
        assert_eq!(live_value(&mt, b"empty"), Some(Vec::new()));
        let weird_k: &[u8] = &[0x00, 0xFF, 0xFE];
        let weird_v: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];
        mt.put(weird_k, weird_v);
        assert_eq!(live_value(&mt, weird_k), Some(weird_v.to_vec()));
    }

    // The live view of `iter()` must be sorted by key, tombstones excluded,
    // latest value per key.
    pub(crate) fn memtable_ordering_contract<M: Memtable + Default>() {
        let mut mt = M::default();

        // Insert out of key order to prove the output is sorted, not insertion-ordered.
        mt.put(b"banana", b"2");
        mt.put(b"apple", b"1");
        mt.put(b"cherry", b"3");
        mt.put(b"date", b"4");

        mt.delete(b"cherry"); // deleted -> excluded
        mt.put(b"apple", b"1b"); // overwritten -> latest value

        let got = live_entries(&mt);
        let expected: Vec<(Vec<u8>, Vec<u8>)> = vec![
            (b"apple".to_vec(), b"1b".to_vec()),
            (b"banana".to_vec(), b"2".to_vec()),
            (b"date".to_vec(), b"4".to_vec()),
        ];
        assert_eq!(
            got, expected,
            "the live view must be sorted by key, with the latest value per key"
        );

        let empty = M::default();
        assert!(live_entries(&empty).is_empty());
    }

    // `clear()` must leave the memtable EMPTY and STILL USABLE.
    //
    // "Still usable" is the part that needs asserting. A skip list keeps a
    // sentinel head node at index 0 that every traversal starts from, and
    // `clear` must rebuild it. An implementation that drops all nodes including
    // the sentinel passes an "is it empty?" check and then panics on the next
    // operation with an out-of-bounds index. So every assertion below that
    // exercises the memtable AFTER a clear is load-bearing, not redundant.
    pub(crate) fn memtable_clear_contract<M: Memtable + Default>() {
        let mut mt = M::default();

        // Clearing an already-empty memtable is a no-op, not an error.
        mt.clear();
        assert!(
            live_entries(&mt).is_empty(),
            "clear on empty leaves it empty"
        );
        assert!(all_entries(&mt).is_empty());
        assert_eq!(live_value(&mt, b"anything"), None);

        // The memtable is usable after clearing an empty memtable.
        mt.put(b"k", b"v");
        assert_eq!(
            live_value(&mt, b"k"),
            Some(b"v".to_vec()),
            "put/get must work after clearing an empty memtable"
        );

        // Populate with enough entries that a skip list grows past level 0,
        // plus a tombstone, then clear.
        for i in 0..64u32 {
            let key = format!("key{i:03}").into_bytes();
            mt.put(&key, format!("val{i}").as_bytes());
        }
        mt.delete(b"key007");
        assert!(
            !live_entries(&mt).is_empty(),
            "precondition: memtable is populated"
        );

        mt.clear();

        // Empty by both views: live-only and flush (which includes tombstones).
        assert!(live_entries(&mt).is_empty(), "no live entries after clear");
        assert!(
            all_entries(&mt).is_empty(),
            "no entries at all after clear — tombstones cleared too"
        );

        // Reads of previously-present keys now miss, and do not panic.
        assert_eq!(
            live_value(&mt, b"key000"),
            None,
            "cleared key reads as absent"
        );
        assert_eq!(live_value(&mt, b"key063"), None);
        assert_eq!(
            live_value(&mt, b"key007"),
            None,
            "cleared tombstone reads as absent"
        );

        // Every write operation still works. This is what catches a clear that
        // destroyed the structure rather than emptying it.
        mt.put(b"after", b"1");
        assert_eq!(
            live_value(&mt, b"after"),
            Some(b"1".to_vec()),
            "put works after clear"
        );

        mt.put(b"after", b"2");
        assert_eq!(
            live_value(&mt, b"after"),
            Some(b"2".to_vec()),
            "overwrite works after clear"
        );

        mt.delete(b"after");
        assert_eq!(live_value(&mt, b"after"), None, "delete works after clear");

        mt.delete(b"never-existed");
        assert_eq!(
            live_value(&mt, b"never-existed"),
            None,
            "delete of absent key still a no-op"
        );

        // Enough writes to exercise multi-level insertion again, then check the
        // ordered scan is correct — not just non-empty.
        for i in 0..32u32 {
            let key = format!("k{i:03}").into_bytes();
            mt.put(&key, format!("v{i}").as_bytes());
        }
        let entries = live_entries(&mt);
        assert_eq!(entries.len(), 32, "all 32 post-clear entries present");
        let mut sorted = entries.clone();
        sorted.sort();
        assert_eq!(entries, sorted, "ordering still holds after clear");

        // Clearing twice in a row is safe.
        mt.clear();
        mt.clear();
        assert!(live_entries(&mt).is_empty());
        mt.put(b"z", b"z");
        assert_eq!(
            live_value(&mt, b"z"),
            Some(b"z".to_vec()),
            "usable after a double clear"
        );
    }

    // `lookup()` must distinguish the three outcomes of a point lookup, which
    // an `Option<Vec<u8>>` return cannot: `None` would mean both "this key was
    // deleted" and "this key was never written". Those demand opposite actions
    // from `Db::get` — a tombstone stops the search, an absent key continues it
    // into older SSTables. Collapsing them is the resurrection bug.
    //
    // `lookup` is the only read primitive on the trait. The `live_value` helper
    // above derives the live-only view from it, so every assertion in every
    // contract here ultimately exercises `lookup`.
    pub(crate) fn memtable_lookup_contract<M: Memtable + Default>() {
        let mut mt = M::default();

        // Never written -> Absent.
        assert!(
            matches!(mt.lookup(b"never"), EntryState::Absent),
            "a key that was never written is Absent, not Tombstone"
        );

        // Written -> Live, carrying the value.
        mt.put(b"k", b"v");
        match mt.lookup(b"k") {
            EntryState::Live(v) => assert_eq!(v, b"v".to_vec()),
            other => panic!("expected Live(b\"v\"), got {other:?}"),
        }

        // Overwritten -> Live with the latest value.
        mt.put(b"k", b"v2");
        match mt.lookup(b"k") {
            EntryState::Live(v) => assert_eq!(v, b"v2".to_vec(), "latest value wins"),
            other => panic!("expected Live(b\"v2\"), got {other:?}"),
        }

        // Deleted -> Tombstone, NOT Absent. This is the distinction that matters.
        mt.delete(b"k");
        assert!(
            matches!(mt.lookup(b"k"), EntryState::Tombstone),
            "a deleted key is Tombstone, not Absent — Absent would let an older \
             SSTable's live value resurrect the key"
        );

        // Deleting a key that was never written also records a Tombstone, for
        // the same reason: the key may live in a lower layer that must be shadowed.
        mt.delete(b"ghost");
        assert!(
            matches!(mt.lookup(b"ghost"), EntryState::Tombstone),
            "deleting a never-written key still yields Tombstone"
        );

        // Re-put after delete -> Live again.
        mt.put(b"k", b"v3");
        match mt.lookup(b"k") {
            EntryState::Live(v) => assert_eq!(v, b"v3".to_vec()),
            other => panic!("expected Live(b\"v3\") after re-put, got {other:?}"),
        }

        // An empty value is Live with zero bytes — distinct from both Tombstone
        // and Absent. This is why a value cannot be used as its own presence flag.
        mt.put(b"empty", b"");
        match mt.lookup(b"empty") {
            EntryState::Live(v) => assert!(v.is_empty(), "empty value is Live, not Absent"),
            other => panic!("expected Live(b\"\"), got {other:?}"),
        }

        // After a clear, everything is Absent — including keys that held
        // tombstones before the clear.
        mt.clear();
        assert!(matches!(mt.lookup(b"k"), EntryState::Absent));
        assert!(
            matches!(mt.lookup(b"ghost"), EntryState::Absent),
            "a cleared tombstone is Absent, not Tombstone"
        );

        // Everything above uses a handful of keys, which a skip list can hold
        // at level 0 alone. 128 keys grows it several levels, so the lookups
        // below exercise the descend-through-levels search rather than a walk
        // along the bottom. A traversal that drops down at the wrong node fails
        // here and passes everything above.
        let mut populated = M::default();
        for i in 0..128u32 {
            let key = format!("key{i:03}").into_bytes();
            populated.put(&key, format!("val{i}").as_bytes());
        }

        match populated.lookup(b"key064") {
            EntryState::Live(v) => assert_eq!(v, b"val64".to_vec(), "mid-range hit"),
            other => panic!("expected Live(b\"val64\"), got {other:?}"),
        }
        match populated.lookup(b"key000") {
            EntryState::Live(v) => assert_eq!(v, b"val0".to_vec(), "first key"),
            other => panic!("expected Live(b\"val0\"), got {other:?}"),
        }
        match populated.lookup(b"key127") {
            EntryState::Live(v) => assert_eq!(v, b"val127".to_vec(), "last key"),
            other => panic!("expected Live(b\"val127\"), got {other:?}"),
        }

        // A miss must report Absent, never the nearest key. Three ways to miss:
        // below the whole range, above it, and between two neighbours.
        // "aaa" < "key000" because b'a' (0x61) < b'k' (0x6B).
        assert!(
            matches!(populated.lookup(b"aaa"), EntryState::Absent),
            "a key sorting before every entry is Absent"
        );
        assert!(
            matches!(populated.lookup(b"zzz"), EntryState::Absent),
            "a key sorting after every entry is Absent"
        );
        // "key064" < "key064x" < "key065": the first is a prefix of the second,
        // and the second differs from the third at b'4' (0x34) < b'5' (0x35).
        assert!(
            matches!(populated.lookup(b"key064x"), EntryState::Absent),
            "a key sorting between two entries is Absent, not the nearest entry"
        );

        // Keys are opaque bytes, not text. This one also sorts before every
        // other key, since it starts with a 0x00 byte.
        let weird: &[u8] = &[0x00, 0xFF, 0xFE];
        populated.put(weird, b"binary");
        match populated.lookup(weird) {
            EntryState::Live(v) => assert_eq!(v, b"binary".to_vec()),
            other => panic!("expected Live for a non-UTF-8 key, got {other:?}"),
        }
        populated.delete(weird);
        assert!(
            matches!(populated.lookup(weird), EntryState::Tombstone),
            "a non-UTF-8 key can be tombstoned like any other"
        );

        // A tombstone must shadow its own key and nothing else. A delete that
        // corrupts the links around the node it touches shows up here.
        populated.delete(b"key064");
        assert!(matches!(populated.lookup(b"key064"), EntryState::Tombstone));
        assert!(
            matches!(populated.lookup(b"key063"), EntryState::Live(_)),
            "deleting a key must not affect its predecessor"
        );
        assert!(
            matches!(populated.lookup(b"key065"), EntryState::Live(_)),
            "deleting a key must not affect its successor"
        );
    }

    #[test]
    fn btreemap_memtable_satisfies_contract() {
        memtable_contract::<BTreeMapMemtable>();
        memtable_ordering_contract::<BTreeMapMemtable>();
        memtable_clear_contract::<BTreeMapMemtable>();
        memtable_lookup_contract::<BTreeMapMemtable>();
    }
}
