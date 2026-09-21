"""Release-surface smoke and audit test declarations."""

load("@rules_shell//shell:sh_test.bzl", "sh_test")


def release_surface_tests():
    sh_test(
        name = "native_ctx_binary_smoke",
        srcs = ["scripts/bazel-native-ctx-smoke.sh"],
        args = [
            "$(rootpath //crates/ctx-cli:ctx)",
            "$(rootpath //crates/ctx-cli:Cargo.toml)",
            "$(rootpath //:Cargo.toml)",
        ],
        data = [
            "//:Cargo.toml",
            "//crates/ctx-cli:Cargo.toml",
            "//crates/ctx-cli:ctx",
        ],
    )

    sh_test(
        name = "release_binary_string_audit_tests",
        srcs = ["scripts/tests/audit-release-binary-strings-test.sh"],
        data = [
            "scripts/check-release-binary-strings.sh",
            "scripts/tests/fixtures/release-binary-strings/removed-cloud-history.txt",
        ],
        tags = ["non-rust-action"],
    )

    sh_test(
        name = "release_source_surface_audit_tests",
        srcs = ["scripts/tests/audit-release-source-surface-test.sh"],
        data = [
            "scripts/check-release-source-surface.sh",
            "scripts/tests/fixtures/release-source-surface/retained-blame-docs/docs/blame.md.fixture",
            "scripts/tests/fixtures/release-source-surface/retired-attribution-manifest/crates/ctx-attribution-model/Cargo.toml.fixture",
            "scripts/tests/fixtures/release-source-surface/retired-ctx-attribution-derivation-surface/crates/ctx-attribution-derivation/src/lib.rs.fixture",
            "scripts/tests/fixtures/release-source-surface/retired-ctx-attribution-index-surface/crates/ctx-attribution-index/src/lib.rs.fixture",
            "scripts/tests/fixtures/release-source-surface/retired-ctx-attribution-model-surface/crates/ctx-attribution-model/src/lib.rs.fixture",
            "scripts/tests/fixtures/release-source-surface/retired-ctx-attribution-surface/crates/ctx-attribution/src/lib.rs.fixture",
            "scripts/tests/fixtures/release-source-surface/retired-ctx-repository-evidence-surface/crates/ctx-repository-evidence/src/lib.rs.fixture",
            "scripts/tests/fixtures/release-source-surface/retired-uninstall-advice/docs/blame.md.fixture",
            "scripts/tests/fixtures/release-source-surface/mutated-hardcoded-product-crate-version/crates/ctx-history-providers-sqlite-inventory/Cargo.toml.fixture",
            "scripts/tests/fixtures/release-source-surface/retained-upgrade-status/crates/ctx-cli/src/upgrade/command/status.rs.fixture",
            "scripts/tests/fixtures/release-source-surface/retained-workspace-product-crate-version/crates/ctx-history-providers-sqlite-inventory/Cargo.toml.fixture",
            "scripts/tests/fixtures/release-source-surface/retired-command-surfaces/crates/ctx-cli/src/main.rs.fixture",
            "scripts/tests/fixtures/release-source-surface/retired-presentation-command-surfaces/crates/ctx-cli-presentation/src/lib.rs.fixture",
            "scripts/tests/fixtures/release-source-surface/retired-task-documents-command-surface/crates/ctx-history-providers-task-docs/src/lib.rs.fixture",
            "scripts/tests/fixtures/release-source-surface/retired-top-level-uninstall/crates/ctx-cli/src/main.rs.fixture",
            "scripts/tests/fixtures/release-source-surface/retired-update-invocation/crates/ctx-cli/src/main.rs.fixture",
            "scripts/tests/fixtures/release-source-surface/retired-update-route/crates/ctx-cli/src/main.rs.fixture",
        ],
        tags = ["non-rust-action"],
    )
