use std::{
    cell::RefCell,
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use super::*;

std::thread_local! {
    static INVENTORY_OPENS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static INVENTORY_WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static GROUP_DIGEST_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static INVENTORY_DIGEST_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ACTIVE_LEAF_HANDLES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PEAK_LEAF_HANDLES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ACTIVE_DIRECTORY_HANDLES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PEAK_DIRECTORY_HANDLES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static EVENT_FILE_IO_COUNTER: RefCell<Option<EventFileIoCounter>> = const { RefCell::new(None) };
}

#[derive(Debug, Default)]
pub(super) struct EventFileIoAtomicCounts {
    body_reads: AtomicUsize,
    leaf_lookups: AtomicUsize,
}

pub(super) type EventFileIoCounter = Arc<EventFileIoAtomicCounts>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EventFileIoCounts {
    pub inventory_opens: usize,
    pub inventory_walks: usize,
    pub body_reads: usize,
    pub leaf_lookups: usize,
    pub group_digest_builds: usize,
    pub inventory_digest_builds: usize,
    pub peak_transient_leaf_handles: usize,
    pub peak_transient_directory_handles: usize,
    pub active_transient_leaf_handles: usize,
    pub active_transient_directory_handles: usize,
}

pub(super) fn note_inventory_open() {
    INVENTORY_OPENS.with(|value| value.set(value.get().saturating_add(1)));
}

pub(super) fn note_inventory_walk() {
    INVENTORY_WALKS.with(|value| value.set(value.get().saturating_add(1)));
}

pub(super) fn current_event_file_io_counter() -> Option<EventFileIoCounter> {
    EVENT_FILE_IO_COUNTER.with(|counter| counter.borrow().clone())
}

pub(super) fn note_body_read(counter: Option<&EventFileIoCounter>) {
    let counter = current_event_file_io_counter().or_else(|| counter.cloned());
    if let Some(counter) = counter {
        counter.body_reads.fetch_add(1, Ordering::Relaxed);
    }
}

pub(super) fn note_leaf_lookup(counter: Option<&EventFileIoCounter>) {
    let counter = current_event_file_io_counter().or_else(|| counter.cloned());
    if let Some(counter) = counter {
        counter.leaf_lookups.fetch_add(1, Ordering::Relaxed);
    }
}

pub(super) fn note_group_digest_build() {
    GROUP_DIGEST_BUILDS.with(|value| value.set(value.get().saturating_add(1)));
}

pub(super) fn note_inventory_digest_build() {
    INVENTORY_DIGEST_BUILDS.with(|value| value.set(value.get().saturating_add(1)));
}

pub(super) fn note_handle_opened(kind: TransientHandleKind) {
    let (active, peak) = match kind {
        TransientHandleKind::Leaf => (&ACTIVE_LEAF_HANDLES, &PEAK_LEAF_HANDLES),
        TransientHandleKind::Directory => (&ACTIVE_DIRECTORY_HANDLES, &PEAK_DIRECTORY_HANDLES),
    };
    active.with(|active| {
        let next = active.get().saturating_add(1);
        active.set(next);
        peak.with(|peak| peak.set(peak.get().max(next)));
    });
}

pub(super) fn note_handle_closed(kind: TransientHandleKind) {
    let active = match kind {
        TransientHandleKind::Leaf => &ACTIVE_LEAF_HANDLES,
        TransientHandleKind::Directory => &ACTIVE_DIRECTORY_HANDLES,
    };
    active.with(|active| active.set(active.get().saturating_sub(1)));
}

pub(crate) fn count_event_file_io<T>(operation: impl FnOnce() -> T) -> (T, EventFileIoCounts) {
    struct CounterScope(Option<EventFileIoCounter>);

    impl Drop for CounterScope {
        fn drop(&mut self) {
            EVENT_FILE_IO_COUNTER.with(|counter| {
                counter.replace(self.0.take());
            });
        }
    }

    let shared = Arc::new(EventFileIoAtomicCounts::default());
    let previous = EVENT_FILE_IO_COUNTER.with(|counter| counter.replace(Some(Arc::clone(&shared))));
    let scope = CounterScope(previous);
    INVENTORY_OPENS.with(|value| value.set(0));
    INVENTORY_WALKS.with(|value| value.set(0));
    GROUP_DIGEST_BUILDS.with(|value| value.set(0));
    INVENTORY_DIGEST_BUILDS.with(|value| value.set(0));
    ACTIVE_LEAF_HANDLES.with(|value| value.set(0));
    PEAK_LEAF_HANDLES.with(|value| value.set(0));
    ACTIVE_DIRECTORY_HANDLES.with(|value| value.set(0));
    PEAK_DIRECTORY_HANDLES.with(|value| value.set(0));
    let output = operation();
    drop(scope);
    let counts = EventFileIoCounts {
        inventory_opens: INVENTORY_OPENS.with(|value| value.replace(0)),
        inventory_walks: INVENTORY_WALKS.with(|value| value.replace(0)),
        body_reads: shared.body_reads.load(Ordering::Relaxed),
        leaf_lookups: shared.leaf_lookups.load(Ordering::Relaxed),
        group_digest_builds: GROUP_DIGEST_BUILDS.with(|value| value.replace(0)),
        inventory_digest_builds: INVENTORY_DIGEST_BUILDS.with(|value| value.replace(0)),
        peak_transient_leaf_handles: PEAK_LEAF_HANDLES.with(|value| value.replace(0)),
        peak_transient_directory_handles: PEAK_DIRECTORY_HANDLES.with(|value| value.replace(0)),
        active_transient_leaf_handles: ACTIVE_LEAF_HANDLES.with(|value| value.replace(0)),
        active_transient_directory_handles: ACTIVE_DIRECTORY_HANDLES.with(|value| value.replace(0)),
    };
    (output, counts)
}

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
    let mut group_instance = PathBuf::new();
    for component in components.iter().take(group_index.saturating_add(1)) {
        group_instance.push(component.as_os_str());
    }
    let relative_file_key = components[group_index + 1..]
        .iter()
        .map(|component| component.as_os_str().to_str().unwrap())
        .collect::<Vec<_>>()
        .join("/");
    Ok(Some(EventFileCoordinates {
        group_key,
        group_instance_key: group_instance.to_string_lossy().into_owned(),
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
    assert_eq!(first.ordinal(), 0);
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
    assert_eq!(counts.active_transient_leaf_handles, 0);
    assert_eq!(counts.active_transient_directory_handles, 0);
    assert_eq!(counts.peak_transient_leaf_handles, 1);
    assert!(counts.peak_transient_directory_handles <= 3);

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
    assert_eq!(counts.active_transient_leaf_handles, 0);
    assert_eq!(counts.active_transient_directory_handles, 0);
    assert_eq!(group.leaves()[0].group_ordinal(), 0);
    assert_eq!(group.leaves()[0].leaf_ordinal(), 0);
    let (bytes, counts) = count_event_file_io(|| group.read_leaf_at(0).unwrap());
    assert_eq!(bytes, b"b");
    assert_eq!(counts.body_reads, 1);
    assert_eq!(counts.peak_transient_leaf_handles, 1);
    assert_eq!(counts.active_transient_leaf_handles, 0);
}

#[test]
fn two_thousand_leaf_inventory_has_a_constant_descriptor_working_set() {
    const LEAF_COUNT: usize = 2_000;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("large-events");
    let conversation = root.join("conversation-large");
    fs::create_dir_all(&conversation).unwrap();
    for index in 0..LEAF_COUNT {
        fs::write(
            conversation.join(format!("{index:04}.json")),
            index.to_string(),
        )
        .unwrap();
    }
    let large_limits = EventFileLimits {
        max_depth: 8,
        max_entries: LEAF_COUNT + 8,
        max_path_bytes: 16 * 1024,
        max_record_bytes: 1024,
    };

    let ((), counts) = count_event_file_io(|| {
        let inventory =
            EventFileInventory::open(&root, large_limits, classify).expect("large inventory");
        assert_eq!(inventory.retained_authority_handles(), 1);
        let group = inventory.groups().next().expect("large group");
        assert_eq!(group.leaves().len(), LEAF_COUNT);
        let group_digest = group.observation_digest();
        assert_eq!(group.observation_digest(), group_digest);
        let inventory_digest = inventory.observation_digest();
        assert_eq!(inventory.observation_digest(), inventory_digest);
        inventory
            .revalidate_all()
            .expect("metadata-only terminal inventory");
    });

    assert_eq!(counts.inventory_opens, 1);
    assert_eq!(counts.inventory_walks, 2);
    assert_eq!(counts.body_reads, 0);
    assert_eq!(counts.group_digest_builds, 2);
    assert_eq!(counts.inventory_digest_builds, 1);
    assert_eq!(counts.peak_transient_leaf_handles, 1);
    assert!(counts.peak_transient_directory_handles <= 2);
    assert_eq!(counts.active_transient_leaf_handles, 0);
    assert_eq!(counts.active_transient_directory_handles, 0);
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
fn changed_leaf_is_rejected_before_reopened_body_read() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("read-race");
    let conversation = root.join("conversation-a");
    fs::create_dir_all(&conversation).unwrap();
    let event = conversation.join("event.json");
    fs::write(&event, b"trusted").unwrap();

    let inventory = EventFileInventory::open(&root, limits(), classify).unwrap();
    let group = inventory.groups().next().unwrap();
    std::thread::sleep(Duration::from_millis(2));
    fs::write(&event, b"changed").unwrap();

    let (result, counts) = count_event_file_io(|| group.read_leaf_at(0));
    assert!(matches!(
        result,
        Err(EventFileInventoryError::SourceChanged { .. })
    ));
    assert_eq!(counts.body_reads, 0);
    assert_eq!(counts.peak_transient_leaf_handles, 1);
    assert_eq!(counts.active_transient_leaf_handles, 0);
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

#[test]
fn duplicate_group_keys_from_distinct_physical_instances_fail_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("duplicate-groups");
    let first = root.join("left").join("conversation-same");
    let second = root.join("right").join("conversation-same");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    fs::write(first.join("first.json"), b"{}").unwrap();
    fs::write(second.join("second.json"), b"{}").unwrap();

    assert!(matches!(
        EventFileInventory::open(&root, limits(), classify),
        Err(EventFileInventoryError::DuplicateGroupInstance { group_key })
            if group_key == "conversation-same"
    ));
}
