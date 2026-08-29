use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{}: not a valid sediment SSTable ({detail})", path.display())]
    CorruptSsTable { path: PathBuf, detail: String },
}

pub type Result<T> = std::result::Result<T, Error>;
