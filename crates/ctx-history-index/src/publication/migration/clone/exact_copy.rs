use std::io::{Read, Write};

use crate::{IndexError, Result};

pub(super) fn copy_exact_authenticated_file<R: Read, W: Write>(
    source: &mut R,
    destination: &mut W,
    expected_bytes: u64,
    aggregate_allowance: u64,
) -> Result<u64> {
    if expected_bytes > aggregate_allowance {
        return Err(IndexError::PredecessorMigrationByteLimit {
            actual: expected_bytes,
            maximum: aggregate_allowance,
        });
    }
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while copied < expected_bytes {
        let remaining = expected_bytes - copied;
        let read_limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| IndexError::CountOverflow)?;
        let read = source.read(&mut buffer[..read_limit])?;
        if read == 0 {
            return Err(IndexError::PredecessorMigrationSourceTopology(
                "source file truncated while cloning",
            ));
        }
        destination.write_all(&buffer[..read])?;
        copied = copied
            .checked_add(read as u64)
            .ok_or(IndexError::CountOverflow)?;
    }
    let mut growth_probe = [0_u8; 1];
    if source.read(&mut growth_probe)? != 0 {
        return Err(IndexError::PredecessorMigrationSourceTopology(
            "source file grew while cloning",
        ));
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    struct InfiniteReader {
        read_bytes: usize,
    }

    impl Read for InfiniteReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            buffer.fill(b'x');
            self.read_bytes += buffer.len();
            Ok(buffer.len())
        }
    }

    #[test]
    fn authenticated_copy_stops_at_the_expected_length_before_growth_rejection() {
        let mut source = InfiniteReader { read_bytes: 0 };
        let mut destination = Vec::new();
        assert!(matches!(
            copy_exact_authenticated_file(&mut source, &mut destination, 17, 17),
            Err(IndexError::PredecessorMigrationSourceTopology(
                "source file grew while cloning"
            ))
        ));
        assert_eq!(destination, vec![b'x'; 17]);
        assert_eq!(source.read_bytes, 18);
    }
}
