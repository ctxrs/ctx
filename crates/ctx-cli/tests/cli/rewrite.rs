#[allow(unused_imports)]
use super::*;

#[cfg(unix)]
pub(crate) fn rewrite_fake_release_metadata(
    release: &FakeRelease,
    rewrite: impl FnOnce(String) -> String,
) {
    let next = rewrite(fs::read_to_string(&release.metadata).unwrap());
    fs::write(&release.metadata, &next).unwrap();
    fs::write(
        &release.signature,
        format!("{}\n", sign_test_release_metadata(next.as_bytes())),
    )
    .unwrap();
}
