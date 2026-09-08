use super::*;

fn isolated_root() -> Result<tempfile::TempDir> {
    #[cfg(target_os = "linux")]
    let parent = std::path::PathBuf::from("/tmp");
    #[cfg(not(target_os = "linux"))]
    let parent = std::env::temp_dir();
    Ok(tempfile::Builder::new()
        .prefix("ctx-private-directory-")
        .tempdir_in(parent)?)
}

#[cfg(target_os = "linux")]
mod creation_race;
#[cfg(target_os = "linux")]
mod syscalls;

#[test]
fn missing_components_are_private_and_external_ancestors_are_unchanged() -> Result<()> {
    let temp = isolated_root()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755))?;
    }
    let root = temp.path().join("data");
    let jobs = root.join("daemon/jobs");
    create_private_dir_all_before_ack(&jobs)?;
    for path in [&root, &root.join("daemon"), &jobs] {
        ctx_history_platform::platform_security::verify_private_directory(path)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&root, &root.join("daemon"), &jobs] {
            assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, 0o700);
        }
        assert_eq!(
            fs::metadata(temp.path())?.permissions().mode() & 0o777,
            0o755
        );
    }
    Ok(())
}

#[test]
fn concurrent_creators_and_retry_preserve_the_same_private_directory() -> Result<()> {
    let temp = isolated_root()?;
    let jobs = temp.path().join("data/daemon/jobs");
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        let threads = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    create_private_dir_all(&jobs)
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap().unwrap();
        }
    });
    let marker = jobs.join("retained");
    fs::write(&marker, "retained")?;
    assert!(create_private_dir_all(&marker.join("blocked")).is_err());
    create_private_dir_all_before_ack(&jobs)?;
    assert_eq!(fs::read_to_string(&marker)?, "retained");
    Ok(())
}

#[cfg(unix)]
#[test]
fn preexisting_readonly_ancestor_is_not_repaired() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let temp = isolated_root()?;
    let external = temp.path().join("external");
    let jobs = external.join("data/daemon/jobs");
    create_private_dir_all(&jobs)?;
    fs::set_permissions(&external, fs::Permissions::from_mode(0o500))?;
    let result = create_private_dir_all(&jobs);
    let mode = fs::metadata(&external)?.permissions().mode() & 0o777;
    fs::set_permissions(&external, fs::Permissions::from_mode(0o700))?;
    result?;
    assert_eq!(mode, 0o500);
    Ok(())
}
