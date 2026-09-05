use std::io::{Read, Write};
use thiserror::Error;

pub(crate) struct Record {
    pub(crate) key: Vec<u8>,
    pub(crate) val: Option<Vec<u8>>, // None = tombstone
}

#[derive(Debug, Error)]
pub(crate) enum DecodeError {
    #[error("{detail}")]
    Incomplete { detail: String },
    #[error("Invalid tag {tag} in record for {:?}", key)]
    InvalidTag { key: Vec<u8>, tag: u8 },
    #[error("I/O error: {0}")]
    Io(std::io::Error),
}

type Result<T> = std::result::Result<T, DecodeError>;

impl Record {
    pub(crate) fn encoded_len(&self) -> u64 {
        4 + self.key.len() as u64 + 1 + 4 + self.val.as_ref().map_or(0, Vec::len) as u64
    }

    pub(crate) fn encode(
        writer: &mut impl Write,
        key: &[u8],
        val: Option<&[u8]>,
    ) -> std::io::Result<()> {
        let key_len = u32::try_from(key.len()).expect("key exceeded 4 GB");
        writer.write_all(&key_len.to_le_bytes())?;
        writer.write_all(key)?;

        let (tag, val_bytes): (u8, &[u8]) = match val {
            Some(v) => (0, v),
            None => (1, &[]),
        };
        let val_len = u32::try_from(val_bytes.len()).expect("val exceeded 4 GB");
        writer.write_all(&[tag])?;
        writer.write_all(&val_len.to_le_bytes())?;
        writer.write_all(val_bytes)?;

        Ok(())
    }

    pub(crate) fn decode(reader: &mut impl Read) -> Result<Record> {
        let mut buffer = [0u8; 4];
        Self::read_exact(reader, &mut buffer)?;
        let key_len = u32::from_le_bytes(buffer);

        let mut key = vec![0u8; key_len as usize];
        Self::read_exact(reader, &mut key)?;

        let mut buffer = [0u8; 1];
        Self::read_exact(reader, &mut buffer)?;
        let tag = buffer[0];

        let mut buffer = [0u8; 4];
        Self::read_exact(reader, &mut buffer)?;
        let value_len = u32::from_le_bytes(buffer);

        let mut val = vec![0u8; value_len as usize];
        Self::read_exact(reader, &mut val)?;

        match tag {
            0 => Ok(Record {
                key,
                val: Some(val),
            }),
            1 => Ok(Record { key, val: None }),
            tag => Err(DecodeError::InvalidTag { key, tag }),
        }
    }

    fn read_exact(reader: &mut impl Read, buf: &mut [u8]) -> Result<()> {
        reader.read_exact(buf).map_err(|e| match e.kind() {
            std::io::ErrorKind::UnexpectedEof => DecodeError::Incomplete {
                detail: format!("record ends mid-stream, wanted {} more bytes", buf.len()),
            },
            _ => DecodeError::Io(e),
        })
    }
}
