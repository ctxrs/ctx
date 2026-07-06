#[allow(unused_imports)]
use super::*;

pub(crate) fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| {
        let value = value.to_string_lossy();
        !matches!(value.as_ref(), "" | "0" | "false" | "False" | "FALSE")
    })
}

pub(crate) fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.parse().ok()
}

pub(crate) fn env_f64(name: &str) -> Option<f64> {
    std::env::var(name).ok()?.parse().ok()
}
