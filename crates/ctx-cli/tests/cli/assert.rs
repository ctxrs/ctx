#[allow(unused_imports)]
use super::*;

pub(crate) fn assert_contains_markers(label: &str, value: &str, expected_markers: &[&str]) {
    for expected in expected_markers {
        assert!(
            value.contains(expected),
            "{label} did not preserve local marker {expected} in {value}"
        );
    }
}
