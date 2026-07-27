use std::{env, ffi::OsStr};

const UNSTAMPED_COMMIT: &str = "0000000000000000000000000000000000000000";
const UNSTAMPED_LOCK: &str = "0000000000000000000000000000000000000000000000000000000000000000";

const SOURCE_COMMIT: &str = match option_env!("CTX_RELEASE_BUILD_SOURCE_COMMIT") {
    Some(value) => value,
    None => UNSTAMPED_COMMIT,
};
const CARGO_LOCK_SHA256: &str = match option_env!("CTX_RELEASE_BUILD_CARGO_LOCK_SHA256") {
    Some(value) => value,
    None => UNSTAMPED_LOCK,
};
const TARGET: &str = match option_env!("CTX_RELEASE_BUILD_TARGET") {
    Some(value) => value,
    None => "unstamped",
};

pub(crate) fn print_if_requested() -> bool {
    let mut args = env::args_os();
    let _program = args.next();
    if args.next().as_deref() != Some(OsStr::new("_release-build-identity")) {
        return false;
    }
    if args.next().is_some() {
        return false;
    }

    println!("CTX_RELEASE_BUILD_SOURCE_COMMIT={SOURCE_COMMIT}");
    println!("CTX_RELEASE_BUILD_CARGO_LOCK_SHA256={CARGO_LOCK_SHA256}");
    println!("CTX_RELEASE_BUILD_TARGET={TARGET}");
    true
}
