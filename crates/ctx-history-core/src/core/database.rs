#[allow(unused_imports)]
use super::*;

pub(crate) fn database_url_password_regex() -> Option<&'static Regex> {
    static REGEX: OnceLock<Option<Regex>> = OnceLock::new();
    REGEX
        .get_or_init(|| {
            Regex::new(
                r#"(?i)\b((?:postgres|postgresql|mysql|mariadb|mongodb|redis)://[^/\s:@]+:)[^/\s@]+@"#,
            )
            .ok()
        })
        .as_ref()
}
