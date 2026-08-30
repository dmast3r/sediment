use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{}: not a valid sediment SSTable ({detail})", path.display())]
    CorruptSsTable { path: PathBuf, detail: String },
    #[error("key is {len} bytes, exceeding the {} byte maximum", u32::MAX)]
    KeyTooLong { len: usize },
    #[error("value is {len} bytes, exceeding the {} byte maximum", u32::MAX)]
    ValueTooLong { len: usize },
    #[error("directory {} is already in use by another process", path.display())]
    DatabaseDirectoryAlreadyInUse { path: PathBuf },
}

pub type Result<T> = std::result::Result<T, Error>;
