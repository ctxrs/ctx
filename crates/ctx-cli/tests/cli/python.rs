#[allow(unused_imports)]
use super::*;

pub(crate) fn python_command() -> String {
    std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_owned())
}
