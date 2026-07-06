#[allow(unused_imports)]
use super::*;

pub(crate) fn ctx(temp: &TempDir) -> Command {
    let mut command = Command::cargo_bin("ctx").unwrap();
    apply_hermetic_env(&mut command, temp);
    command
}
