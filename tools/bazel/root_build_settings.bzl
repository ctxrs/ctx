"""Small, stable settings declared in the repository's root package."""


def root_build_settings():
    """Declares root package data and public build configurations."""
    native.filegroup(
        name = "cloud_removed_build_inputs",
        srcs = [
            "Cargo.toml",
            "//crates/ctx-agent-application:Cargo.toml",
            "//crates/ctx-agent-integrations:Cargo.toml",
            "//crates/ctx-cli:Cargo.toml",
            "//crates/ctx-cli-presentation:Cargo.toml",
            "//crates/ctx-daemon-cli:Cargo.toml",
            "//crates/ctx-daemon-application:Cargo.toml",
            "//crates/ctx-daemon-refresh-client:Cargo.toml",
            "//crates/ctx-daemon-runtime:Cargo.toml",
            "//crates/ctx-daemon-service:Cargo.toml",
        ],
        visibility = ["//visibility:public"],
    )

    native.config_setting(
        name = "dev_linux",
        define_values = {"ctx_dev_linux": "true"},
        visibility = ["//visibility:public"],
    )

    native.config_setting(
        name = "release_freebsd",
        constraint_values = ["@platforms//os:freebsd"],
        values = {"compilation_mode": "opt"},
        visibility = ["//visibility:public"],
    )
