use super::*;

#[cfg(unix)]
#[test]
fn replacing_the_bound_root_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("root");
    let displaced = directory.path().join("displaced");
    fs::create_dir(&root)?;
    let binding = BoundRepositoryRoot::authorize(&root)?;
    fs::rename(&root, &displaced)?;
    fs::create_dir(&root)?;

    assert!(binding.verify().is_err());
    Ok(())
}

#[test]
fn replacing_a_bound_descendant_file_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("root");
    let file = root.join("tracked.txt");
    let displaced = root.join("displaced.txt");
    fs::create_dir(&root)?;
    fs::write(&file, "original")?;
    let binding = BoundRepositoryRoot::authorize(&root)?;
    let bound_file = binding.existing_file(Path::new("tracked.txt"))?;
    fs::rename(&file, &displaced)?;
    fs::write(&file, "replacement")?;

    assert!(bound_file.verify().is_err());
    Ok(())
}

#[test]
fn oversized_and_excessively_deep_paths_fail_before_traversal()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let binding = BoundRepositoryRoot::authorize(directory.path())?;
    let oversized = PathBuf::from("a".repeat(MAX_REPOSITORY_PATH_BYTES + 1));
    let deep = (0..=MAX_REPOSITORY_PATH_COMPONENTS).fold(PathBuf::new(), |path, _| path.join("a"));

    assert!(binding.existing_file(&oversized).is_err());
    assert!(binding.existing_file(&deep).is_err());
    Ok(())
}
