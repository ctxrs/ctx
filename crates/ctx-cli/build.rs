use std::env;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(ctx_semantic_fastembed)");
    println!("cargo:rustc-check-cfg=cfg(ctx_sqlite_vec)");

    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let fastembed_supported =
        os == "linux" && (arch == "x86_64" || arch == "aarch64") && target_env == "gnu";

    if fastembed_supported {
        println!("cargo:rustc-cfg=ctx_semantic_fastembed");
    }

    if fastembed_supported {
        println!("cargo:rustc-cfg=ctx_sqlite_vec");
    }
}
