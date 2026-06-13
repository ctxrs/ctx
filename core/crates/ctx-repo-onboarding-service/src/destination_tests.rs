use super::*;

#[tokio::test]
async fn prepare_repo_init_path_rejects_non_empty_without_opt_in() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("repo");
    tokio::fs::create_dir_all(&dir).await.expect("mkdir");
    tokio::fs::write(dir.join("README.md"), "hello")
        .await
        .expect("write");

    let error = prepare_repo_init_path(RepoInitPathRequest {
        path: dir.to_str().expect("utf8 path"),
        allow_existing: true,
        allow_non_empty: false,
    })
    .await
    .expect_err("non-empty dir");

    assert_eq!(
        error.message(),
        format!("destination is not empty: {}", dir.display())
    );
}

#[tokio::test]
async fn validate_repo_destination_requires_empty_when_requested() {
    let tmp = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(tmp.path().join("README.md"), "hello")
        .await
        .expect("write");

    let error = validate_repo_destination(RepoValidateDestinationRequest {
        path: tmp.path().to_str().expect("utf8 path"),
        must_not_exist: false,
        require_empty_if_exists: true,
    })
    .await
    .expect_err("non-empty destination");

    assert_eq!(
        error.message(),
        format!("destination is not empty: {}", tmp.path().display())
    );
}

#[tokio::test]
async fn prepare_clone_destination_derives_repo_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = prepare_clone_destination(RepoCloneDestinationRequest {
        repo_url: "git@github.com:org/repo.git",
        dest_parent: tmp.path().to_str().expect("utf8 path"),
        dest_name: None,
    })
    .await
    .expect("clone destination");

    assert_eq!(dest, tmp.path().join("repo"));
}
