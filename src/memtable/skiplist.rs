use crate::lookup::EntryState;
use crate::memtable::Value::{Live, Tombstone};
use crate::memtable::{Memtable, Value};
use rand::prelude::StdRng;
use rand::{Rng, SeedableRng};

const MAX_LEVEL: usize = 20;

struct Node {
    key: Vec<u8>,
    val: Value,
    next: [Option<usize>; MAX_LEVEL],
}

pub struct SkipListMemtable {
    nodes: Vec<Node>,
    rng: StdRng,
}

impl SkipListMemtable {
    const HEAD_INDEX: usize = 0;

    fn with_rng(rng: StdRng) -> Self {
        let head_node = Self::build_head_node();
        SkipListMemtable {
            nodes: vec![head_node],
            rng,
        }
    }

    // Returns (predecessors at each level, candidate = first node with key >= `key`).
    // The candidate is always a forward `next` pointer, so head is never returned as the candidate
    fn find(&self, key: &[u8]) -> ([usize; MAX_LEVEL], Option<usize>) {
        let mut previous_nodes = [Self::HEAD_INDEX; MAX_LEVEL];
        let mut node_index = Self::HEAD_INDEX;

        for level in (0..MAX_LEVEL).rev() {
            while let Some(next_index) = self.nodes[node_index].next[level] {
                if self.nodes[next_index].key.as_slice() >= key {
                    break;
                }
                node_index = next_index;
            }
            previous_nodes[level] = node_index;
        }

        (previous_nodes, self.nodes[previous_nodes[0]].next[0])
    }

    fn find_exact(&self, key: &[u8]) -> ([usize; MAX_LEVEL], Option<usize>) {
        let (previous_nodes, candidate_index) = self.find(key);
        (
            previous_nodes,
            candidate_index.filter(|&index| self.nodes[index].key.as_slice() == key),
        )
    }

    fn upsert(&mut self, key: &[u8], val: Value) {
        let (previous_nodes, existing) = self.find_exact(key);

        if let Some(index) = existing {
            self.nodes[index].val = val;
            return;
        }

        let mut node = Node {
            key: key.to_vec(),
            val,
            next: [None; MAX_LEVEL],
        };
        let mut level = 0;
        let new_index = self.nodes.len();

        while level < MAX_LEVEL && (level == 0 || self.rng.random::<bool>()) {
            node.next[level] = self.nodes[previous_nodes[level]].next[level];
            self.nodes[previous_nodes[level]].next[level] = Some(new_index);
            level += 1;
        }

        self.nodes.push(node);
    }

    fn build_head_node() -> Node {
        Node {
            key: vec![],
            val: Live(vec![]),
            next: [None; MAX_LEVEL],
        }
    }
}

struct SkipListIterator<'a> {
    nodes: &'a [Node],
    curr_index: Option<usize>,
}

impl<'a> Iterator for SkipListIterator<'a> {
    type Item = (&'a [u8], Option<&'a [u8]>);

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(index) = self.curr_index {
            let node = &self.nodes[index];
            let val = match &node.val {
                Live(v) => Some(v.as_slice()),
                Tombstone => None,
            };
            self.curr_index = node.next[0];
            Some((node.key.as_slice(), val))
        } else {
            None
        }
    }
}

impl Memtable for SkipListMemtable {
    fn put(&mut self, key: &[u8], val: &[u8]) {
        self.upsert(key, Live(val.to_vec()));
    }

    fn delete(&mut self, key: &[u8]) {
        self.upsert(key, Tombstone);
    }

    fn lookup(&self, key: &[u8]) -> EntryState {
        self.find_exact(key)
            .1
            .map_or(EntryState::Absent, |index| match &self.nodes[index].val {
                Live(val) => EntryState::Live(val.clone()),
                Tombstone => EntryState::Tombstone,
            })
    }

    fn iter(&self) -> impl Iterator<Item = (&[u8], Option<&[u8]>)> {
        SkipListIterator {
            nodes: &self.nodes,
            curr_index: self.nodes[Self::HEAD_INDEX].next[0],
        }
    }

    fn clear(&mut self) {
        self.nodes = vec![Self::build_head_node()];
    }
}

impl Default for SkipListMemtable {
    fn default() -> Self {
        Self::with_rng(StdRng::from_os_rng())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memtable::btree::BTreeMapMemtable;
    use crate::memtable::contract::{
        memtable_clear_contract, memtable_contract, memtable_lookup_contract,
        memtable_ordering_contract,
    };
    use crate::memtable::{Memtable, SkipListMemtable};

    /// Seeded constructor so randomized runs are reproducible. Randomized impls
    /// (the skip list) must seed from it; deterministic ones (BTreeMap) ignore it.
    pub trait TestConstruct {
        fn new_for_test(seed: u64) -> Self;
    }

    impl TestConstruct for BTreeMapMemtable {
        fn new_for_test(_seed: u64) -> Self {
            Self::default()
        }
    }

    impl TestConstruct for SkipListMemtable {
        fn new_for_test(seed: u64) -> Self {
            Self::with_rng(StdRng::seed_from_u64(seed))
        }
    }

    /// Deterministic op-stream generator (xorshift64) so the differential test
    /// is reproducible without pulling `rand` into the test.
    struct OpRng(u64);
    impl OpRng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    /// Model-based test: drive the same random op stream through the impl under
    /// test and the trusted `BTreeMapMemtable`, then assert `entries()` agree.
    /// Any structural bug in the skip list surfaces as divergence from the
    /// reference — a stronger net than hand-picked cases.
    fn differential_against_btreemap<M: Memtable + TestConstruct>() {
        let mut subject = M::new_for_test(0xDEAD_BEEF);
        let mut reference = BTreeMapMemtable::new_for_test(0xDEAD_BEEF);

        let mut rng = OpRng(0x1234_5678_9ABC_DEF0);
        // Small key space so overwrites and deletes actually collide.
        for _ in 0..2_000 {
            let r = rng.next();
            let key = format!("key{:03}", r % 64).into_bytes();
            if r % 4 == 0 {
                subject.delete(&key);
                reference.delete(&key);
            } else {
                let val = format!("val{}", r % 1000).into_bytes();
                subject.put(&key, &val);
                reference.put(&key, &val);
            }
        }

        // Compare EVERY entry, tombstones included. `iter()` makes the stronger
        // comparison the natural one: a skip list that mishandled a tombstone
        // but produced the right live entries would now be caught.
        let subject_all: Vec<_> = subject
            .iter()
            .map(|(k, v)| (k.to_vec(), v.map(|v| v.to_vec())))
            .collect();
        let reference_all: Vec<_> = reference
            .iter()
            .map(|(k, v)| (k.to_vec(), v.map(|v| v.to_vec())))
            .collect();
        assert_eq!(
            subject_all, reference_all,
            "skip list diverged from BTreeMap reference on identical op stream"
        );
    }

    #[test]
    fn skiplist_satisfies_contract() {
        memtable_contract::<SkipListMemtable>();
        memtable_ordering_contract::<SkipListMemtable>();
        memtable_clear_contract::<SkipListMemtable>();
        memtable_lookup_contract::<SkipListMemtable>();
    }

    #[test]
    fn skiplist_matches_btreemap_differentially() {
        differential_against_btreemap::<SkipListMemtable>();
    }
}
