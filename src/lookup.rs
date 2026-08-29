#[derive(Debug)]
pub enum EntryState {
    Live(Vec<u8>),
    Tombstone,
    Absent,
}
