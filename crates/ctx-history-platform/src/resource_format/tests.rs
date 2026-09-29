use super::*;

#[test]
fn ordinary_byte_format_retains_progress_conventions() {
    for (bytes, expected) in [
        (0, "0 B"),
        (1, "1 B"),
        (1023, "1023 B"),
        (1024, "1.0 KiB"),
        (1025, "1.0 KiB"),
        (1536, "1.5 KiB"),
        (1_048_576, "1.0 MiB"),
        (1_073_741_824, "1.0 GiB"),
        (1_099_511_627_776, "1.0 TiB"),
        (u64::MAX, "16777216.0 TiB"),
    ] {
        assert_eq!(format_bytes(bytes), expected, "{bytes}");
    }
}

#[test]
fn disk_shortage_reports_the_additional_space_to_free() {
    assert_eq!(
        format_disk_shortage(8_685_611_961, 8_558_231_552),
        "free at least 121.5 MiB; required 8.1 GiB; available 8.0 GiB"
    );
    assert_eq!(
        format_disk_shortage(1_073_741_825, 1_073_741_824),
        "free at least 1 B; required 1.0 GiB; available 1.0 GiB"
    );
    assert_eq!(
        format_disk_shortage(u64::MAX, u64::MAX - 1),
        "free at least 1 B; required 16777216.0 TiB; available 16777216.0 TiB"
    );
    assert_eq!(
        format_disk_shortage(0, u64::MAX),
        "free at least 0 B; required 0 B; available 16777216.0 TiB"
    );
    assert_eq!(
        format_disk_shortage(1024, 1024),
        "free at least 0 B; required 1.0 KiB; available 1.0 KiB"
    );
}

#[test]
fn upward_rounding_never_understates_the_deficit() {
    for (bytes, expected) in [
        (1, "1 B"),
        (1023, "1023 B"),
        (1024, "1.0 KiB"),
        (1025, "1.1 KiB"),
        (1536, "1.5 KiB"),
        (1537, "1.6 KiB"),
        (1_048_575, "1024.0 KiB"),
        (1_048_576, "1.0 MiB"),
        (1_048_577, "1.1 MiB"),
        (1_073_741_823, "1024.0 MiB"),
        (1_073_741_824, "1.0 GiB"),
        (1_073_741_825, "1.1 GiB"),
        (1_099_511_627_775, "1024.0 GiB"),
        (1_099_511_627_776, "1.0 TiB"),
        (1_099_511_627_777, "1.1 TiB"),
        (u64::MAX, "16777216.0 TiB"),
    ] {
        assert_eq!(format_bytes_up(bytes), expected, "{bytes}");
        assert!(format_disk_shortage(bytes, 0).starts_with(&format!("free at least {expected};")));
    }
}
