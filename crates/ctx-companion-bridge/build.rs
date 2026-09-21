fn main() {
    println!("cargo:rustc-check-cfg=cfg(ctx_release_qualification)");
}
