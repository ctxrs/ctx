use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }
    for input in [
        "../ctx-task-launcher",
        "../../Cargo.toml",
        "../../Cargo.lock",
    ] {
        println!("cargo:rerun-if-changed={input}");
    }
    let target = env::var("TARGET").unwrap();
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("launcher-target");
    // The outer Cargo process holds its target directory lock while this runs.
    let status = Command::new(env::var_os("CARGO").unwrap())
        .env("CARGO_BUILD_BUILD_DIR", &out)
        .args([
            "build",
            "--locked",
            "--release",
            "-p",
            "ctx-task-launcher",
            "--target",
            &target,
            "--target-dir",
        ])
        .arg(&out)
        .status()
        .expect("run Cargo for the Windows task launcher");
    assert!(
        status.success(),
        "failed to build the Windows task launcher"
    );
    let launcher = out.join(target).join("release/ctx-task-launcher.exe");
    println!("cargo:rustc-env=CTX_TASK_LAUNCHER={}", launcher.display());
}
