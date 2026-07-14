use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use crate::common::io::visit_jsonl_paths;
use crate::common::scratch::CaptureScratchSpace;
use crate::Result;

const PATH_INSERT_BATCH: usize = 128;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SortedPathInventoryMetrics {
    pub(crate) paths: usize,
    pub(crate) max_in_memory_batch: usize,
}

pub(crate) struct SortedJsonlPathInventory {
    connection: Connection,
    _scratch: CaptureScratchSpace,
    metrics: SortedPathInventoryMetrics,
}

impl SortedJsonlPathInventory {
    pub(crate) fn build(root: &Path, mut include: impl FnMut(&Path) -> bool) -> Result<Self> {
        let scratch = CaptureScratchSpace::create("path-inventory")?;
        drop(scratch.create_file("paths.sqlite")?);
        let mut connection = Connection::open(scratch.path().join("paths.sqlite"))?;
        connection.execute_batch(
            "PRAGMA journal_mode = MEMORY;
             PRAGMA synchronous = OFF;
             CREATE TABLE paths (
                 sort_key BLOB PRIMARY KEY NOT NULL,
                 path BLOB NOT NULL
             ) WITHOUT ROWID;",
        )?;

        let mut pending = Vec::with_capacity(PATH_INSERT_BATCH);
        let mut metrics = SortedPathInventoryMetrics::default();
        visit_jsonl_paths(root, &mut |path| {
            if include(path) {
                let encoded = encode_path(path);
                pending.push((encoded.clone(), encoded));
                metrics.max_in_memory_batch = metrics.max_in_memory_batch.max(pending.len());
                if pending.len() == PATH_INSERT_BATCH {
                    metrics.paths += flush_paths(&mut connection, &mut pending)?;
                }
            }
            Ok(())
        })?;
        metrics.paths += flush_paths(&mut connection, &mut pending)?;

        Ok(Self {
            connection,
            _scratch: scratch,
            metrics,
        })
    }

    pub(crate) fn metrics(&self) -> SortedPathInventoryMetrics {
        self.metrics
    }

    pub(crate) fn for_each(&self, mut visitor: impl FnMut(PathBuf) -> Result<()>) -> Result<()> {
        let mut statement = self
            .connection
            .prepare("SELECT path FROM paths ORDER BY sort_key")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let encoded = row.get::<_, Vec<u8>>(0)?;
            visitor(decode_path(encoded)?)?;
        }
        Ok(())
    }
}

fn flush_paths(
    connection: &mut Connection,
    pending: &mut Vec<(Vec<u8>, Vec<u8>)>,
) -> Result<usize> {
    if pending.is_empty() {
        return Ok(0);
    }
    let transaction = connection.transaction()?;
    let mut inserted = 0usize;
    {
        let mut statement =
            transaction.prepare("INSERT OR IGNORE INTO paths (sort_key, path) VALUES (?1, ?2)")?;
        for (sort_key, path) in pending.drain(..) {
            inserted += statement.execute(params![sort_key, path])?;
        }
    }
    transaction.commit()?;
    Ok(inserted)
}

#[cfg(unix)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
fn decode_path(encoded: Vec<u8>) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    Ok(PathBuf::from(std::ffi::OsString::from_vec(encoded)))
}

#[cfg(windows)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_be_bytes)
        .collect()
}

#[cfg(windows)]
fn decode_path(encoded: Vec<u8>) -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    if encoded.len() % 2 != 0 {
        return Err(crate::CaptureError::SystemInvariant(
            "capture path inventory contains an invalid Windows path",
        ));
    }
    let wide = encoded
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&wide)))
}

#[cfg(not(any(unix, windows)))]
fn encode_path(_path: &Path) -> Vec<u8> {
    Vec::new()
}

#[cfg(not(any(unix, windows)))]
fn decode_path(_encoded: Vec<u8>) -> Result<PathBuf> {
    Err(crate::CaptureError::SystemInvariant(
        "capture path inventory is unsupported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn inventory_streams_lexicographically_with_a_bounded_insert_batch() {
        let temp = tempfile::tempdir().unwrap();
        for index in (0..513).rev() {
            fs::write(temp.path().join(format!("path-{index:04}.jsonl")), "{}\n").unwrap();
        }

        let inventory = SortedJsonlPathInventory::build(temp.path(), |_| true).unwrap();
        let mut observed = Vec::new();
        inventory
            .for_each(|path| {
                observed.push(path);
                Ok(())
            })
            .unwrap();

        let mut expected = observed.clone();
        expected.sort();
        assert_eq!(observed, expected);
        assert_eq!(inventory.metrics().paths, 513);
        assert_eq!(inventory.metrics().max_in_memory_batch, PATH_INSERT_BATCH);
    }

    #[cfg(unix)]
    #[test]
    fn inventory_round_trips_non_utf8_paths() {
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(std::ffi::OsString::from_vec(
            b"non-utf8-\xff.jsonl".to_vec(),
        ));
        fs::write(&path, "{}\n").unwrap();

        let inventory = SortedJsonlPathInventory::build(temp.path(), |_| true).unwrap();
        let mut observed = Vec::new();
        inventory
            .for_each(|path| {
                observed.push(path);
                Ok(())
            })
            .unwrap();
        assert_eq!(observed, vec![path]);
    }
}
