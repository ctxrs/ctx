use anyhow::Result;
use std::path::Path;

pub(super) fn set_raw_tar_path(header: &mut tar::Header, raw_path: &[u8]) {
    assert!(raw_path.len() < 100, "test tar path must fit old header");
    let bytes = header.as_mut_bytes();
    bytes[0..100].fill(0);
    bytes[0..raw_path.len()].copy_from_slice(raw_path);
}

pub(super) fn write_tar_gz(
    path: &Path,
    write_entries: impl FnOnce(&mut tar::Builder<flate2::write::GzEncoder<std::fs::File>>) -> Result<()>,
) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    write_entries(&mut builder)?;
    let encoder = builder.into_inner()?;
    encoder.finish()?;
    Ok(())
}

#[cfg(unix)]
pub(super) fn path_is_ascii_case_insensitive(path: &Path) -> bool {
    let upper = path.join(format!(".ctx-test-case-probe-{}-A", std::process::id()));
    let lower = path.join(format!(".ctx-test-case-probe-{}-a", std::process::id()));
    if upper.exists() || lower.exists() {
        return false;
    }
    std::fs::File::create(&upper).expect("create case sensitivity probe");
    let is_case_insensitive = lower.exists();
    std::fs::remove_file(&upper).expect("remove case sensitivity probe");
    is_case_insensitive
}

pub(super) fn patch_zip_entry_unix_mode(zip_path: &Path, entry_name: &str, mode: u32) {
    let mut bytes = std::fs::read(zip_path).expect("read zip for patching");
    let mut offset = 0usize;
    while offset + 46 <= bytes.len() {
        let relative = bytes[offset..]
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .expect("central directory header");
        let start = offset + relative;
        assert!(
            start + 46 <= bytes.len(),
            "central directory header truncated"
        );
        let name_len = u16::from_le_bytes([bytes[start + 28], bytes[start + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[start + 30], bytes[start + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[start + 32], bytes[start + 33]]) as usize;
        let name_start = start + 46;
        let name_end = name_start + name_len;
        assert!(name_end <= bytes.len(), "central directory name truncated");
        if &bytes[name_start..name_end] == entry_name.as_bytes() {
            bytes[start + 5] = 3;
            bytes[start + 38..start + 42].copy_from_slice(&(mode << 16).to_le_bytes());
            std::fs::write(zip_path, bytes).expect("write patched zip");
            return;
        }
        offset = name_end + extra_len + comment_len;
    }
    panic!("zip entry not found: {entry_name}");
}
