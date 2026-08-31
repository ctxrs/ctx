use std::{fs, path::Path};

use super::compiled::{
    commit_compile_destination, invalidate_compiled_model_cache, prepare_compile_destination,
    AtomicCommit,
};
#[cfg(unix)]
use super::compiled::{discard_compile_destination, validate_compiled_model_cache};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

#[test]
fn runtime_owns_compiled_cache_atomic_publication_and_winner_reuse() {
    let temp = tempfile::tempdir().unwrap();
    let hash = "b".repeat(64);
    let first =
        prepare_compile_destination(temp.path(), &hash, "document", "coremltools-8.3").unwrap();
    write(&first.staging_path.join("model.bin"), b"compiled");
    assert_eq!(
        commit_compile_destination(&first).unwrap(),
        AtomicCommit::Installed
    );
    assert_eq!(
        fs::read(first.final_path.join("model.bin")).unwrap(),
        b"compiled"
    );

    let second =
        prepare_compile_destination(temp.path(), &hash, "document", "coremltools-8.3").unwrap();
    write(&second.staging_path.join("model.bin"), b"loser");
    assert_eq!(
        commit_compile_destination(&second).unwrap(),
        AtomicCommit::AlreadyPresent
    );
    assert!(!second.staging_path.exists());
    assert_eq!(
        fs::read(second.final_path.join("model.bin")).unwrap(),
        b"compiled"
    );
}

#[test]
fn runtime_invalidates_corrupt_compiled_cache_once() {
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("document.mlmodelc");
    fs::create_dir(&cache).unwrap();
    write(&cache.join("corrupt.bin"), b"corrupt");
    invalidate_compiled_model_cache(&cache).unwrap();
    assert!(!cache.exists());
    assert!(invalidate_compiled_model_cache(&cache).is_err());
    assert!(invalidate_compiled_model_cache(&temp.path().join("unexpected")).is_err());
}

#[cfg(unix)]
#[test]
fn runtime_compiled_cache_rejects_symlinked_output_tree() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let destination =
        prepare_compile_destination(temp.path(), &"c".repeat(64), "query", "coremltools-8.3")
            .unwrap();
    symlink("missing", destination.staging_path.join("link")).unwrap();
    assert!(commit_compile_destination(&destination).is_err());
    discard_compile_destination(&destination).unwrap();
}

#[cfg(unix)]
#[test]
fn passive_compiled_cache_rejects_symlinked_managed_ancestor() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let hash = "d".repeat(64);
    let compiler_hash = "existing-compiler-hash";
    let outside_model = outside
        .path()
        .join("sha256")
        .join(&hash)
        .join(compiler_hash)
        .join("document.mlmodelc");
    fs::create_dir_all(&outside_model).unwrap();
    symlink(outside.path(), temp.path().join("coreml-compiled")).unwrap();
    let model = temp
        .path()
        .join("coreml-compiled")
        .join("sha256")
        .join(hash)
        .join(compiler_hash)
        .join("document.mlmodelc");

    assert!(validate_compiled_model_cache(temp.path(), &model).is_err());
}

#[cfg(unix)]
#[test]
fn passive_compiled_cache_rejects_symlink_inside_bundle() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let destination =
        prepare_compile_destination(temp.path(), &"e".repeat(64), "query", "coremltools-8.3")
            .unwrap();
    write(&destination.staging_path.join("model.bin"), b"compiled");
    commit_compile_destination(&destination).unwrap();
    symlink("model.bin", destination.final_path.join("alias.bin")).unwrap();

    assert!(validate_compiled_model_cache(temp.path(), &destination.final_path).is_err());
}
