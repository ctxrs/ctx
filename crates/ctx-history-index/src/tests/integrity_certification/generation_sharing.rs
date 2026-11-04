use super::*;
use std::os::unix::fs::MetadataExt;

#[test]
fn publication_hard_links_unchanged_segments_when_supported() {
    let (temp, source, initial) = published_fixture("shared-generation.jsonl");
    let previous = active_generation_path(temp.path());
    let probe = temp.path().join("hardlink-probe");
    if let Err(error) = fs::hard_link(active_store_path(temp.path()), &probe) {
        eprintln!("filesystem cannot hard-link sealed segments: {error}");
        return;
    }
    fs::remove_file(probe).unwrap();
    // The probe changed the artifact's ctime; authenticate it before publication.
    drop(VerifiedIndex::open_pinned(temp.path()).unwrap());

    append_one_record(temp.path(), &source).unwrap();
    let active = active_generation_path(temp.path());
    let mut shared = 0;
    for entry in fs::read_dir(&previous).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') || name == "meta.json" {
            continue;
        }
        let prior = entry.metadata().unwrap();
        let current = fs::metadata(active.join(&name)).unwrap();
        assert_eq!(
            (prior.dev(), prior.ino()),
            (current.dev(), current.ino()),
            "{name:?}"
        );
        assert!(current.nlink() > 1, "{name:?}");
        assert!(current.permissions().readonly(), "{name:?}");
        shared += 1;
    }
    assert!(shared > 0);
    // Tantivy can rewrite these control files; sharing them would mutate the base.
    for name in ["meta.json", ".managed.json"] {
        assert_ne!(
            fs::metadata(previous.join(name)).unwrap().ino(),
            fs::metadata(active.join(name)).unwrap().ino(),
            "{name} must remain independent"
        );
    }
    let mut reader = VerifiedIndex::open_pinned_with_retained_peer(temp.path()).unwrap();
    let retained = reader
        .take_retained_generation_peer_for_reader()
        .unwrap()
        .unwrap();
    assert_eq!(retained.generation_id(), initial.generation_id);
}
