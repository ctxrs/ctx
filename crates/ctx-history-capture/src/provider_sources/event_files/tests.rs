use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::Path,
    time::Duration,
};

use super::*;

fn limits() -> EventFileLimits {
    EventFileLimits {
        max_depth: 8,
        max_entries: 64,
        max_path_bytes: 16 * 1024,
        max_record_bytes: 1024,
    }
}

fn classify(path: &Path) -> Result<Option<EventFileCoordinates>> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
        return Ok(None);
    }
    let components = path.components().collect::<Vec<_>>();
    let Some(group_index) = components.iter().position(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|value| value.starts_with("conversation-"))
    }) else {
        return Ok(None);
    };
    let group_key = components[group_index]
        .as_os_str()
        .to_str()
        .unwrap()
        .to_owned();
    let relative_file_key = components[group_index + 1..]
        .iter()
        .map(|component| component.as_os_str().to_str().unwrap())
        .collect::<Vec<_>>()
        .join("/");
    Ok(Some(EventFileCoordinates {
        group_key,
        relative_file_key,
    }))
}

#[test]
fn directory_and_exact_file_inventories_are_sorted_and_body_free() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("events");
    fs::create_dir_all(root.join("conversation-b")).unwrap();
    fs::create_dir_all(root.join("conversation-a").join("nested")).unwrap();
    fs::write(root.join("conversation-b").join("z.json"), b"z").unwrap();
    fs::write(
        root.join("conversation-a").join("nested").join("b.json"),
        b"b",
    )
    .unwrap();
    fs::write(root.join("conversation-a").join("a.json"), b"a").unwrap();
    fs::write(root.join("conversation-a").join("ignored.txt"), b"ignored").unwrap();

    let (directory, counts) =
        count_event_file_io(|| EventFileInventory::open(&root, limits(), classify).unwrap());
    assert_eq!(
        directory
            .groups()
            .map(|group| group.group_key().to_owned())
            .collect::<Vec<_>>(),
        vec!["conversation-a", "conversation-b"]
    );
    let first = directory.groups().next().unwrap();
    assert_eq!(
        first
            .leaves()
            .iter()
            .map(|leaf| leaf.coordinates().relative_file_key.as_str())
            .collect::<Vec<_>>(),
        vec!["a.json", "nested/b.json"]
    );
    assert_eq!(counts.inventory_opens, 1);
    assert_eq!(counts.body_reads, 0);

    let exact_path = root.join("conversation-a").join("nested").join("b.json");
    let (exact, counts) =
        count_event_file_io(|| EventFileInventory::open(&exact_path, limits(), classify).unwrap());
    assert!(exact.selected_file());
    let group = exact.groups().next().unwrap();
    assert_eq!(group.group_key(), "conversation-a");
    assert_eq!(
        group.leaves()[0].selected_relative_path(),
        Path::new("b.json")
    );
    assert_eq!(counts.body_reads, 0);
    let (bytes, counts) = count_event_file_io(|| group.read_leaf(&group.leaves()[0]).unwrap());
    assert_eq!(bytes, b"b");
    assert_eq!(counts.body_reads, 1);
}

#[test]
fn existing_empty_is_authoritative_but_missing_is_typed_unavailable() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let empty = temp.path().join("empty");
    fs::create_dir(&empty).unwrap();

    let inventory = EventFileInventory::open(&empty, limits(), classify).unwrap();
    assert!(inventory.is_empty());
    assert_eq!(inventory.groups().len(), 0);
    inventory.revalidate_all().unwrap();

    assert!(matches!(
        EventFileInventory::open(&temp.path().join("missing"), limits(), classify),
        Err(EventFileInventoryError::Unavailable { .. })
    ));
}

#[test]
fn inventory_enforces_depth_entry_path_and_record_bounds() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("bounded");
    fs::create_dir_all(root.join("conversation-a")).unwrap();
    fs::write(root.join("conversation-a").join("a.json"), b"1234").unwrap();
    fs::write(root.join("conversation-a").join("b.json"), b"12").unwrap();
    fs::write(root.join("conversation-a").join("c.json"), b"12").unwrap();

    let mut bounded = limits();
    bounded.max_depth = 0;
    assert!(matches!(
        EventFileInventory::open(&root, bounded, classify),
        Err(EventFileInventoryError::LimitExceeded {
            limit: EventFileLimit::Depth,
            ..
        })
    ));

    bounded = limits();
    bounded.max_entries = 1;
    assert!(matches!(
        EventFileInventory::open(&root, bounded, classify),
        Err(EventFileInventoryError::LimitExceeded {
            limit: EventFileLimit::Entries,
            ..
        })
    ));

    bounded = limits();
    bounded.max_path_bytes = root.to_str().unwrap().len().saturating_sub(1);
    assert!(matches!(
        EventFileInventory::open(&root, bounded, classify),
        Err(EventFileInventoryError::LimitExceeded {
            limit: EventFileLimit::PathBytes,
            ..
        })
    ));

    bounded = limits();
    bounded.max_record_bytes = 3;
    assert!(matches!(
        EventFileInventory::open(&root, bounded, classify),
        Err(EventFileInventoryError::RecordTooLarge { .. })
    ));
}

#[cfg(unix)]
#[test]
fn inventory_rejects_non_utf8_symlink_and_nonregular_components() {
    use std::{
        ffi::{CString, OsString},
        os::unix::{ffi::OsStringExt, fs::symlink},
    };

    let temp = crate::test_support_paths::tempdir().unwrap();
    let non_utf_root = temp.path().join("non-utf");
    fs::create_dir(&non_utf_root).unwrap();
    fs::write(
        non_utf_root.join(OsString::from_vec(vec![b'f', 0xff, b'.', b'j'])),
        b"x",
    )
    .unwrap();
    assert!(matches!(
        EventFileInventory::open(&non_utf_root, limits(), classify),
        Err(EventFileInventoryError::InvalidPath { .. })
    ));

    let target = temp.path().join("target.json");
    fs::write(&target, b"{}").unwrap();
    let exact_link = temp.path().join("conversation-link.json");
    symlink(&target, &exact_link).unwrap();
    assert!(matches!(
        EventFileInventory::open(&exact_link, limits(), classify),
        Err(EventFileInventoryError::Unavailable { .. })
    ));

    let target_parent = temp.path().join("target-parent");
    fs::create_dir_all(target_parent.join("conversation-a")).unwrap();
    fs::write(
        target_parent.join("conversation-a").join("event.json"),
        b"{}",
    )
    .unwrap();
    let linked_parent = temp.path().join("linked-parent");
    symlink(&target_parent, &linked_parent).unwrap();
    assert!(matches!(
        EventFileInventory::open(&linked_parent, limits(), classify),
        Err(EventFileInventoryError::Unavailable { .. })
    ));

    let fifo_root = temp.path().join("fifo-root");
    fs::create_dir_all(fifo_root.join("conversation-a")).unwrap();
    let fifo = fifo_root.join("conversation-a").join("event.json");
    let fifo_c = CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    assert!(matches!(
        EventFileInventory::open(&fifo_root, limits(), classify),
        Err(EventFileInventoryError::Unavailable { .. })
    ));
}

#[test]
fn retained_inventory_rejects_leaf_add_delete_and_same_size_rewrite() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("mutations");
    let conversation = root.join("conversation-a");
    fs::create_dir_all(&conversation).unwrap();
    let event = conversation.join("event.json");
    fs::write(&event, b"aaaa").unwrap();

    let added = EventFileInventory::open(&root, limits(), classify).unwrap();
    std::thread::sleep(Duration::from_millis(2));
    fs::write(conversation.join("added.json"), b"new").unwrap();
    assert!(matches!(
        added.revalidate_all(),
        Err(EventFileInventoryError::SourceChanged { .. })
    ));
    fs::remove_file(conversation.join("added.json")).unwrap();

    let deleted = EventFileInventory::open(&root, limits(), classify).unwrap();
    std::thread::sleep(Duration::from_millis(2));
    fs::remove_file(&event).unwrap();
    assert!(matches!(
        deleted.revalidate_all(),
        Err(EventFileInventoryError::SourceChanged { .. })
    ));
    fs::write(&event, b"aaaa").unwrap();

    let rewritten = EventFileInventory::open(&root, limits(), classify).unwrap();
    let original_modified = fs::metadata(&event).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(2));
    let mut file = fs::File::options().write(true).open(&event).unwrap();
    file.seek(SeekFrom::Start(1)).unwrap();
    file.write_all(b"b").unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(original_modified))
        .unwrap();
    drop(file);
    assert!(matches!(
        rewritten.revalidate_all(),
        Err(EventFileInventoryError::SourceChanged { .. })
    ));
}

#[test]
fn retained_inventory_rejects_directory_and_selected_root_replacement() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("root");
    let conversation = root.join("conversation-a");
    fs::create_dir_all(&conversation).unwrap();
    fs::write(conversation.join("event.json"), b"trusted").unwrap();

    let directory_swap = EventFileInventory::open(&root, limits(), classify).unwrap();
    fs::rename(&conversation, root.join("old-conversation")).unwrap();
    fs::create_dir(&conversation).unwrap();
    fs::write(conversation.join("event.json"), b"replacement").unwrap();
    assert!(matches!(
        directory_swap.revalidate_all(),
        Err(EventFileInventoryError::SourceChanged { .. })
    ));

    fs::remove_dir_all(&root).unwrap();
    fs::create_dir_all(root.join("conversation-a")).unwrap();
    fs::write(root.join("conversation-a").join("event.json"), b"trusted").unwrap();
    let root_swap = EventFileInventory::open(&root, limits(), classify).unwrap();
    let displaced = temp.path().join("displaced");
    fs::rename(&root, &displaced).unwrap();
    fs::create_dir_all(root.join("conversation-a")).unwrap();
    fs::write(
        root.join("conversation-a").join("event.json"),
        b"replacement",
    )
    .unwrap();
    assert!(matches!(
        root_swap.revalidate_all(),
        Err(EventFileInventoryError::SourceChanged { .. })
    ));
}

#[test]
fn cheap_group_observation_detects_rewrite_without_body_reads() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("observations");
    let conversation = root.join("conversation-a");
    fs::create_dir_all(&conversation).unwrap();
    let event = conversation.join("event.json");
    fs::write(&event, b"aaaa").unwrap();
    let original_modified = fs::metadata(&event).unwrap().modified().unwrap();

    let (before, before_counts) =
        count_event_file_io(|| EventFileInventory::open(&root, limits(), classify).unwrap());
    let before_digest = before.groups().next().unwrap().observation_digest();
    drop(before);

    std::thread::sleep(Duration::from_millis(2));
    fs::write(&event, b"bbbb").unwrap();
    let file = fs::File::options().write(true).open(&event).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(original_modified))
        .unwrap();
    drop(file);

    let (after, after_counts) =
        count_event_file_io(|| EventFileInventory::open(&root, limits(), classify).unwrap());
    let after_digest = after.groups().next().unwrap().observation_digest();
    assert_ne!(before_digest, after_digest);
    assert_eq!(before_counts.body_reads, 0);
    assert_eq!(after_counts.body_reads, 0);
}

#[test]
fn exact_selection_rejects_a_non_event_file_instead_of_claiming_empty_authority() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let selected = temp.path().join("ordinary.json");
    fs::write(&selected, b"{}").unwrap();

    assert!(matches!(
        EventFileInventory::open(&selected, limits(), classify),
        Err(EventFileInventoryError::NoAcceptedExactFile { .. })
    ));
}
