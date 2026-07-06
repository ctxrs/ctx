#[allow(unused_imports)]
use super::*;

pub(crate) fn one_line_error(error: &str) -> String {
    error
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown error")
        .to_owned()
}
