#[allow(unused_imports)]
use super::*;

pub(crate) fn local_cli_markers() -> &'static [&'static str] {
    &[
        "sk-fake00000000000000000000000000000000000000000000",
        "AKIAFAKE000000000000",
        "fake.jwt.token",
        "fake_password",
        "fake_secret_value",
        "fake-password-123",
        "fake_token@git.example.com",
        "person@example.invalid",
    ]
}
