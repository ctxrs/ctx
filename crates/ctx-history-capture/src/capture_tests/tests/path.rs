#[allow(unused_imports)]
use super::*;

pub(crate) fn write_oversized_jsonl_line(path: &Path) {
    fs::write(path, vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES + 1]).unwrap();
}

pub(crate) fn copy_dir_all(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let entry_path = entry.path();
        let target = to.join(entry.file_name());
        if entry_path.is_dir() {
            copy_dir_all(&entry_path, &target);
        } else {
            fs::copy(entry_path, target).unwrap();
        }
    }
}

pub(crate) fn write_unimportable_jsonl_siblings(root: &Path, prefix: &str) {
    fs::write(root.join(format!("{prefix}-empty.jsonl")), "").unwrap();
    fs::write(
        root.join(format!("{prefix}-malformed.jsonl")),
        "{\"not valid\"\n",
    )
    .unwrap();
    fs::write(
        root.join(format!("{prefix}-headerless.jsonl")),
        "{\"type\":\"message\",\"content\":\"missing session header\"}\n",
    )
    .unwrap();
}
