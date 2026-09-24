use super::*;

pub(crate) fn append_query_log(path: &Path, record: &serde_json::Value) -> Result<()> {
    use std::io::{BufRead, Read, Seek};
    let mut bytes = serde_json::to_vec(record)?;
    ensure!(bytes.len() <= 1024 * 1024, "query log record exceeds 1 MiB");
    bytes.push(b'\n');
    let mut options = std::fs::OpenOptions::new();
    options.read(true).append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        ensure!(metadata.is_file(), "query log must be a regular file");
    }
    let mut file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "query log must be a regular file"
    );
    if file.metadata()?.len() > 0 {
        let mut first = Vec::new();
        std::io::BufReader::new((&mut file).take(1024 * 1024 + 1)).read_until(b'\n', &mut first)?;
        ensure!(
            serde_json::from_slice::<serde_json::Value>(&first)?.is_object(),
            "existing log must contain JSON objects"
        );
        file.seek(std::io::SeekFrom::End(-1))?;
        let mut tail = [0];
        file.read_exact(&mut tail)?;
        if tail[0] != b'\n' {
            bytes.insert(0, b'\n');
        }
    }
    file.write_all(&bytes)?;
    Ok(())
}
