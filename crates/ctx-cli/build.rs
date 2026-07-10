use std::env;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(ctx_semantic_fastembed)");
    println!("cargo:rustc-check-cfg=cfg(ctx_sqlite_vec)");

    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let fastembed_supported = public_semantic_platform(&os, &arch, &target_env);

    if fastembed_supported {
        println!("cargo:rustc-cfg=ctx_semantic_fastembed");
    }

    if fastembed_supported {
        println!("cargo:rustc-cfg=ctx_sqlite_vec");
    }
}

fn public_semantic_platform(os: &str, arch: &str, target_env: &str) -> bool {
    match (os, arch) {
        ("linux", "x86_64" | "aarch64") => target_env == "gnu",
        ("macos", "x86_64" | "aarch64") => true,
        ("windows", "x86_64") => true,
        ("freebsd", "x86_64") => true,
        _ => false,
    }
}
