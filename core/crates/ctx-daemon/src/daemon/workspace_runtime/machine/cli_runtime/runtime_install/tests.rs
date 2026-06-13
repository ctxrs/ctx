use std::collections::HashMap;

use super::paths::{
    managed_sandbox_cli_runtime_bin_path, managed_sandbox_cli_runtime_is_ready,
    managed_sandbox_cli_runtime_root,
};
use super::*;

#[tokio::test]
async fn partial_managed_sandbox_cli_runtime_triggers_repair_instead_of_reuse() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = bundled_assets::ManagedRuntimeSource {
        uri: "http://127.0.0.1:9/sandbox-cli-runtime.tar.gz".to_string(),
        sha256: "deadbeef".to_string(),
        version: "test-version".to_string(),
        bin: "bin/sandbox-cli".to_string(),
        helpers: HashMap::from([(
            "gvproxy".to_string(),
            bundled_assets::ManagedArtifactSource {
                uri: "http://127.0.0.1:9/gvproxy".to_string(),
                sha256: "deadbeef".to_string(),
            },
        )]),
    };

    let runtime_root = managed_sandbox_cli_runtime_root(temp.path(), &source);
    let runtime_bin = managed_sandbox_cli_runtime_bin_path(temp.path(), &source);
    fs::create_dir_all(runtime_bin.parent().expect("runtime bin parent"))
        .await
        .expect("create runtime bin parent");
    fs::write(&runtime_bin, b"partial-sandbox-cli")
        .await
        .expect("write partial runtime binary");

    let err =
        ensure_managed_sandbox_cli_runtime_with_override(temp.path(), Some(&source), None, None)
            .await
            .expect_err("partial runtime should trigger repair attempt");

    assert!(
        !managed_sandbox_cli_runtime_is_ready(&runtime_root, &runtime_bin, &source),
        "missing ready marker/helper payload must not count as a ready runtime"
    );
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains("downloading managed artifact")
            || rendered.contains("managed artifact download http error"),
        "repair should attempt managed runtime download, got: {rendered}"
    );
}
