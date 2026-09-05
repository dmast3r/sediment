use crate::error::{Error, Result};
use crate::record::{DecodeError, Record};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};

pub(crate) struct Wal {
    file: File,
    path: PathBuf,
}

impl Wal {
    const WAL_FILE_NAME: &str = "wal";

    pub(crate) fn open<P: AsRef<Path>>(dir: P) -> Result<Self> {
        let path = dir.as_ref().join(Self::WAL_FILE_NAME);
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)?;
        Ok(Wal { file, path })
    }

    pub(crate) fn append(&mut self, key: &[u8], value: Option<&[u8]>) -> Result<()> {
        let mut buf = Vec::new();
        Record::encode(&mut buf, key, value)?;
        self.file.write_all(&buf)?;
        Ok(())
    }

    pub(crate) fn replay(&self) -> Result<Vec<Record>> {
        let file_len = self.file.metadata()?.len();
        let mut reader = BufReader::new(&self.file);

        let mut bytes_read = 0;
        let mut records = Vec::new();

        while bytes_read < file_len {
            let record = match Record::decode(&mut reader) {
                Ok(record) => record,
                Err(DecodeError::Incomplete { .. }) => {
                    self.file.set_len(bytes_read)?;
                    break;
                }
                Err(DecodeError::Io(err)) => return Err(Error::Io(err)),
                Err(err @ DecodeError::InvalidTag { .. }) => {
                    return Err(Error::CorruptWal {
                        detail: err.to_string(),
                        path: self.path.clone(),
                    });
                }
            };

            bytes_read += record.encoded_len();
            records.push(record);
        }

        Ok(records)
    }

    pub(crate) fn reset(&mut self) -> Result<()> {
        self.file.set_len(0)?;
        Ok(())
    }
}
