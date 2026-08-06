fn main() {
    println!("cargo:rustc-check-cfg=cfg(ctx_release_qualification)");
    println!("cargo:rustc-check-cfg=cfg(ctx_pro_test_helper)");
    println!("cargo:rustc-check-cfg=cfg(ctx_cli_test_support_fixtures)");
    println!("cargo:rustc-check-cfg=cfg(ctx_cli_test_support_pro)");
    println!("cargo:rustc-check-cfg=cfg(ctx_cli_test_support_upgrade)");
    println!("cargo:rustc-check-cfg=cfg(ctx_cli_bazel_test)");
}
