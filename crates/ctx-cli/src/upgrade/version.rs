use std::cmp::Ordering;

use anyhow::{anyhow, Context, Result};
use semver::Version;

pub(super) fn parse_semver(value: &str) -> Result<Version> {
    Version::parse(value).map_err(|error| anyhow!("invalid SemVer {value:?}: {error}"))
}

pub(super) fn version_gt(left: &str, right: &str) -> bool {
    compare_precedence(left, right).is_ok_and(Ordering::is_gt)
}

fn versions_exactly_equal(left: &Version, right: &str) -> bool {
    parse_semver(right).is_ok_and(|right| left == &right)
}

#[derive(Debug, Clone)]
pub(super) struct CtxBinaryVersion {
    reported: String,
    parsed: Version,
}

impl CtxBinaryVersion {
    pub(super) fn parse(output: &[u8]) -> Result<Self> {
        let parsed = parse_ctx_version_output(output)?;
        Ok(Self {
            reported: format!("ctx {parsed}"),
            parsed,
        })
    }

    pub(super) fn matches_exactly(&self, expected: &str) -> bool {
        versions_exactly_equal(&self.parsed, expected)
    }

    pub(super) fn trim(&self) -> &str {
        &self.reported
    }
}

fn parse_ctx_version_output(output: &[u8]) -> Result<Version> {
    let output = std::str::from_utf8(output).context("ctx --version output is not UTF-8")?;
    let line = output
        .strip_suffix("\r\n")
        .or_else(|| output.strip_suffix('\n'))
        .unwrap_or(output);
    if line.contains(['\r', '\n']) {
        return Err(anyhow!(
            "ctx --version output must contain exactly one line"
        ));
    }
    let value = line
        .strip_prefix("ctx ")
        .ok_or_else(|| anyhow!("ctx --version output must be exactly `ctx <SEMVER>`"))?;
    parse_semver(value).context("ctx --version reported an invalid version")
}

fn compare_precedence(left: &str, right: &str) -> Result<Ordering> {
    let left = parse_semver(left)?;
    let right = parse_semver(right)?;
    Ok(left.cmp_precedence(&right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_precedence_orders_prereleases_exactly() {
        let ordered = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for pair in ordered.windows(2) {
            assert!(
                version_gt(pair[1], pair[0]),
                "{} should have higher precedence than {}",
                pair[1],
                pair[0]
            );
            assert!(!version_gt(pair[0], pair[1]));
        }
    }

    #[test]
    fn build_metadata_is_exact_for_identity_but_ignored_for_precedence() {
        let left = parse_semver("1.2.3-rc.1+linux.1").unwrap();
        assert!(versions_exactly_equal(&left, "1.2.3-rc.1+linux.1"));
        assert!(!versions_exactly_equal(&left, "1.2.3-rc.1+linux.2"));
        assert_eq!(
            compare_precedence("1.2.3+build.2", "1.2.3+build.1").unwrap(),
            Ordering::Equal
        );
        assert!(!version_gt("1.2.3+build.2", "1.2.3+build.1"));
        assert!(!version_gt("1.2.3+build.1", "1.2.3+build.2"));
    }

    #[test]
    fn arbitrary_digit_bearing_strings_are_not_versions() {
        for invalid in ["v1.2.3", "ctx 1.2.3", "release-9.9.9", "1.2", "01.2.3"] {
            assert!(!version_gt(invalid, "0.0.0"), "{invalid:?}");
            assert!(parse_semver(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn exact_ctx_version_output_accepts_semver() {
        for (output, expected) in [
            (&b"ctx 1.2.3"[..], "1.2.3"),
            (&b"ctx 1.2.3\n"[..], "1.2.3"),
            (&b"ctx 1.2.3-rc.1+linux.7\r\n"[..], "1.2.3-rc.1+linux.7"),
        ] {
            assert_eq!(
                parse_ctx_version_output(output).unwrap().to_string(),
                expected
            );
        }
    }

    #[test]
    fn exact_ctx_version_output_rejects_lookalikes_and_extra_text() {
        for output in [
            &b"ctx 11.2.30\nextra 1.2.3\n"[..],
            &b"ctx 1.2.3 extra\n"[..],
            &b"not-ctx 1.2.3\n"[..],
            &b"ctx v1.2.3\n"[..],
            &b"ctx 1.2.3\n\n"[..],
            &b"ctx 1.2.3 \n"[..],
            &b"ctx 1.2\n"[..],
            &b"\xff\xfe"[..],
        ] {
            assert!(parse_ctx_version_output(output).is_err(), "{output:?}");
        }
    }

    #[test]
    fn staged_version_match_is_exact_including_build_metadata() {
        let version = CtxBinaryVersion::parse(b"ctx 11.2.30+linux.1\n").unwrap();
        assert!(version.matches_exactly("11.2.30+linux.1"));
        assert!(!version.matches_exactly("1.2.3"));
        assert!(!version.matches_exactly("11.2.30+linux.2"));
        assert!(!version.matches_exactly("ctx 11.2.30+linux.1"));
    }
}
