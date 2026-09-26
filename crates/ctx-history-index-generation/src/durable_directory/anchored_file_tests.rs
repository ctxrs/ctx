use super::*;
use tempfile::tempdir;

#[test]
fn overlapping_slices_share_backing_and_outlive_the_handle() {
    let temporary = tempdir().unwrap();
    let path = temporary.path().join("segment.term");
    fs::write(&path, b"abcdefgh").unwrap();
    let handle = AnchoredFileHandle::new(File::open(&path).unwrap()).unwrap();

    let left = handle.read_bytes(1..6).unwrap();
    let right = handle.read_bytes(3..8).unwrap();
    assert_eq!(left.as_slice(), b"bcdef");
    assert_eq!(right.as_slice(), b"defgh");
    assert_eq!(left.as_ptr().wrapping_add(2), right.as_ptr());
    let nested = left.slice(2..5);
    assert_eq!(nested.as_ptr(), right.as_ptr());

    drop(handle);
    drop(left);
    drop(right);
    assert_eq!(nested.as_slice(), b"def");
}

#[cfg(unix)]
#[test]
fn opened_file_handles_and_slices_survive_path_replacement_and_unlink() {
    let temporary = tempdir().unwrap();
    let path = temporary.path().join("segment.term");
    let displaced = temporary.path().join("displaced.term");
    fs::write(&path, b"original").unwrap();
    let file = File::open(&path).unwrap();
    let handle = AnchoredFileHandle::new(file.try_clone().unwrap()).unwrap();
    let slice = handle.read_bytes(1..5).unwrap();

    fs::rename(&path, &displaced).unwrap();
    fs::write(&path, b"replacement").unwrap();
    // Mapping after pathname replacement must still use the opened file.
    let later_handle = AnchoredFileHandle::new(file).unwrap();
    fs::remove_file(&displaced).unwrap();
    assert_eq!(handle.read_bytes(0..8).unwrap().as_slice(), b"original");
    assert_eq!(
        later_handle.read_bytes(0..8).unwrap().as_slice(),
        b"original"
    );
    assert_eq!(fs::read(&path).unwrap(), b"replacement");

    fs::remove_file(&path).unwrap();
    drop(handle);
    drop(later_handle);
    assert_eq!(slice.as_slice(), b"rigi");
}

#[test]
fn empty_files_and_range_bounds_preserve_read_semantics() {
    let temporary = tempdir().unwrap();
    for contents in [b"".as_slice(), b"abc".as_slice()] {
        let path = temporary.path().join("segment.term");
        fs::write(&path, contents).unwrap();
        let handle = AnchoredFileHandle::new(File::open(&path).unwrap()).unwrap();
        let len = contents.len();
        assert_eq!(handle.len(), len);
        assert_eq!(handle.read_bytes(0..len).unwrap().as_slice(), contents);
        assert!(handle.read_bytes(0..0).unwrap().is_empty());
        assert!(handle.read_bytes(len..len).unwrap().is_empty());
        for range in [
            std::ops::Range { start: 1, end: 0 },
            0..len + 1,
            len + 1..len + 1,
            0..usize::MAX,
        ] {
            assert_eq!(
                handle.read_bytes(range).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }
}

#[test]
fn anchored_directory_reads_without_permitting_writes() {
    let temporary = tempdir().unwrap();
    let path = temporary.path().canonicalize().unwrap();
    ctx_history_platform::platform_security::ensure_private_directory(&path).unwrap();
    fs::write(path.join("segment.term"), b"immutable").unwrap();
    let root = crate::GenerationReadRoot::open_index_root(&path).unwrap();
    let directory = crate::read_root::with_registered_read_root(&root, || {
        DurableMmapDirectory::open(root.path())
    })
    .unwrap();
    assert!(matches!(
        &directory.inner,
        DurableDirectoryBackend::Anchored(_)
    ));

    let file = Path::new("segment.term");
    let slice = directory.open_read(file).unwrap().read_bytes().unwrap();
    assert!(directory.open_write(Path::new("new.term")).is_err());
    assert_eq!(
        directory.atomic_write(file, b"changed").unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(directory.delete(file).is_err());
    assert_eq!(fs::read(path.join(file)).unwrap(), b"immutable");
    assert!(!path.join("new.term").exists());
    drop(directory);
    drop(root);
    assert_eq!(slice.as_slice(), b"immutable");
}
