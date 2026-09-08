use serde_json::{Map, Value};

use super::{storage_bytes_bucket, StorageBytesBucket};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilesystemAvailableFractionBucketV1 {
    Zero,
    UnderFivePercent,
    FiveToTenPercent,
    TenToTwentyPercent,
    TwentyToFortyPercent,
    FortyToSixtyPercent,
    AtLeastSixtyPercent,
}

impl FilesystemAvailableFractionBucketV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Zero => "0",
            Self::UnderFivePercent => "lt_5pct",
            Self::FiveToTenPercent => "5pct-10pct",
            Self::TenToTwentyPercent => "10pct-20pct",
            Self::TwentyToFortyPercent => "20pct-40pct",
            Self::FortyToSixtyPercent => "40pct-60pct",
            Self::AtLeastSixtyPercent => "60pct+",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoreLogicalAmplificationBucketV1 {
    UnderPointOne,
    PointOneToPointTwoFive,
    PointTwoFiveToPointThreeFive,
    PointThreeFiveToPointFive,
    PointFiveToOne,
    OneToTwo,
    AtLeastTwo,
}

impl CoreLogicalAmplificationBucketV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::UnderPointOne => "lt_0_10x",
            Self::PointOneToPointTwoFive => "0_10x-0_25x",
            Self::PointTwoFiveToPointThreeFive => "0_25x-0_35x",
            Self::PointThreeFiveToPointFive => "0_35x-0_50x",
            Self::PointFiveToOne => "0_50x-1x",
            Self::OneToTwo => "1x-2x",
            Self::AtLeastTwo => "2x+",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilesystemAvailableToActiveCoreRatioBucketV1 {
    UnderPointFive,
    PointFiveToOne,
    OneToOnePointTwoFive,
    OnePointTwoFiveToTwo,
    TwoToFour,
    AtLeastFour,
}

impl FilesystemAvailableToActiveCoreRatioBucketV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::UnderPointFive => "lt_0_5x",
            Self::PointFiveToOne => "0_5x-1x",
            Self::OneToOnePointTwoFive => "1x-1_25x",
            Self::OnePointTwoFiveToTwo => "1_25x-2x",
            Self::TwoToFour => "2x-4x",
            Self::AtLeastFour => "4x+",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FilesystemStorageFactsV1 {
    total: StorageBytesBucket,
    available: StorageBytesBucket,
    available_fraction: FilesystemAvailableFractionBucketV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CoreStorageFactsV1 {
    active_logical: StorageBytesBucket,
    certified_source: StorageBytesBucket,
    logical_amplification: Option<CoreLogicalAmplificationBucketV1>,
}

/// Sparse, content-free storage facts for daemon ready and liveness events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonStorageFactsV1 {
    filesystem: Option<FilesystemStorageFactsV1>,
    core: Option<CoreStorageFactsV1>,
    available_to_active_core: Option<FilesystemAvailableToActiveCoreRatioBucketV1>,
}

impl DaemonStorageFactsV1 {
    pub fn from_exact(filesystem: Option<(u64, u64)>, core: Option<(u64, u64)>) -> Option<Self> {
        let filesystem_exact =
            filesystem.filter(|(total, available)| *total > 0 && available <= total);
        let filesystem = filesystem_exact.map(|(total, available)| FilesystemStorageFactsV1 {
            total: storage_bytes_bucket(total),
            available: storage_bytes_bucket(available),
            available_fraction: filesystem_available_fraction_bucket(available, total),
        });
        let core_exact = core;
        let core = core_exact.map(|(active_logical, certified_source)| CoreStorageFactsV1 {
            active_logical: storage_bytes_bucket(active_logical),
            certified_source: storage_bytes_bucket(certified_source),
            logical_amplification: (certified_source > 0)
                .then(|| core_logical_amplification_bucket(active_logical, certified_source)),
        });
        let available_to_active_core =
            filesystem_exact
                .zip(core_exact)
                .and_then(|((_, available), (active_logical, _))| {
                    (active_logical > 0).then(|| {
                        filesystem_available_to_active_core_ratio_bucket(available, active_logical)
                    })
                });
        (filesystem.is_some() || core.is_some()).then_some(Self {
            filesystem,
            core,
            available_to_active_core,
        })
    }

    pub(super) fn insert_properties(self, properties: &mut Map<String, Value>) {
        if let Some(filesystem) = self.filesystem {
            insert(
                properties,
                "filesystem_total_bytes_bucket",
                filesystem.total.as_str(),
            );
            insert(
                properties,
                "filesystem_available_bytes_bucket",
                filesystem.available.as_str(),
            );
            insert(
                properties,
                "filesystem_available_fraction_bucket",
                filesystem.available_fraction.as_str(),
            );
        }
        if let Some(core) = self.core {
            insert(
                properties,
                "core_active_logical_bytes_bucket",
                core.active_logical.as_str(),
            );
            insert(
                properties,
                "core_certified_source_bytes_bucket",
                core.certified_source.as_str(),
            );
            if let Some(amplification) = core.logical_amplification {
                insert(
                    properties,
                    "core_logical_amplification_bucket",
                    amplification.as_str(),
                );
            }
        }
        if let Some(ratio) = self.available_to_active_core {
            insert(
                properties,
                "filesystem_available_to_active_core_ratio_bucket",
                ratio.as_str(),
            );
        }
    }
}

fn filesystem_available_fraction_bucket(
    available: u64,
    total: u64,
) -> FilesystemAvailableFractionBucketV1 {
    if available == 0 {
        FilesystemAvailableFractionBucketV1::Zero
    } else if ratio_below(available, total, 5, 100) {
        FilesystemAvailableFractionBucketV1::UnderFivePercent
    } else if ratio_below(available, total, 10, 100) {
        FilesystemAvailableFractionBucketV1::FiveToTenPercent
    } else if ratio_below(available, total, 20, 100) {
        FilesystemAvailableFractionBucketV1::TenToTwentyPercent
    } else if ratio_below(available, total, 40, 100) {
        FilesystemAvailableFractionBucketV1::TwentyToFortyPercent
    } else if ratio_below(available, total, 60, 100) {
        FilesystemAvailableFractionBucketV1::FortyToSixtyPercent
    } else {
        FilesystemAvailableFractionBucketV1::AtLeastSixtyPercent
    }
}

fn core_logical_amplification_bucket(
    logical: u64,
    source: u64,
) -> CoreLogicalAmplificationBucketV1 {
    if ratio_below(logical, source, 1, 10) {
        CoreLogicalAmplificationBucketV1::UnderPointOne
    } else if ratio_below(logical, source, 1, 4) {
        CoreLogicalAmplificationBucketV1::PointOneToPointTwoFive
    } else if ratio_below(logical, source, 7, 20) {
        CoreLogicalAmplificationBucketV1::PointTwoFiveToPointThreeFive
    } else if ratio_below(logical, source, 1, 2) {
        CoreLogicalAmplificationBucketV1::PointThreeFiveToPointFive
    } else if ratio_below(logical, source, 1, 1) {
        CoreLogicalAmplificationBucketV1::PointFiveToOne
    } else if ratio_below(logical, source, 2, 1) {
        CoreLogicalAmplificationBucketV1::OneToTwo
    } else {
        CoreLogicalAmplificationBucketV1::AtLeastTwo
    }
}

fn filesystem_available_to_active_core_ratio_bucket(
    available: u64,
    active_core: u64,
) -> FilesystemAvailableToActiveCoreRatioBucketV1 {
    if ratio_below(available, active_core, 1, 2) {
        FilesystemAvailableToActiveCoreRatioBucketV1::UnderPointFive
    } else if ratio_below(available, active_core, 1, 1) {
        FilesystemAvailableToActiveCoreRatioBucketV1::PointFiveToOne
    } else if ratio_below(available, active_core, 5, 4) {
        FilesystemAvailableToActiveCoreRatioBucketV1::OneToOnePointTwoFive
    } else if ratio_below(available, active_core, 2, 1) {
        FilesystemAvailableToActiveCoreRatioBucketV1::OnePointTwoFiveToTwo
    } else if ratio_below(available, active_core, 4, 1) {
        FilesystemAvailableToActiveCoreRatioBucketV1::TwoToFour
    } else {
        FilesystemAvailableToActiveCoreRatioBucketV1::AtLeastFour
    }
}

fn ratio_below(value: u64, denominator: u64, numerator: u64, divisor: u64) -> bool {
    u128::from(value) * u128::from(divisor) < u128::from(denominator) * u128::from(numerator)
}

fn insert(properties: &mut Map<String, Value>, key: &'static str, value: &'static str) {
    properties.insert(key.to_owned(), Value::String(value.to_owned()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_boundaries_are_lower_inclusive_without_floats() {
        for (available, expected) in [
            (0, "0"),
            (4, "lt_5pct"),
            (5, "5pct-10pct"),
            (10, "10pct-20pct"),
            (20, "20pct-40pct"),
            (40, "40pct-60pct"),
            (60, "60pct+"),
        ] {
            assert_eq!(
                filesystem_available_fraction_bucket(available, 100).as_str(),
                expected
            );
        }
    }

    #[test]
    fn amplification_boundaries_are_lower_inclusive_without_floats() {
        for (logical, expected) in [
            (9, "lt_0_10x"),
            (10, "0_10x-0_25x"),
            (25, "0_25x-0_35x"),
            (35, "0_35x-0_50x"),
            (50, "0_50x-1x"),
            (100, "1x-2x"),
            (200, "2x+"),
        ] {
            assert_eq!(
                core_logical_amplification_bucket(logical, 100).as_str(),
                expected
            );
        }
    }

    #[test]
    fn available_to_core_boundaries_are_lower_inclusive_without_floats() {
        for (available, expected) in [
            (49, "lt_0_5x"),
            (50, "0_5x-1x"),
            (100, "1x-1_25x"),
            (125, "1_25x-2x"),
            (200, "2x-4x"),
            (400, "4x+"),
        ] {
            assert_eq!(
                filesystem_available_to_active_core_ratio_bucket(available, 100).as_str(),
                expected
            );
        }
    }

    #[test]
    fn zero_source_preserves_stock_fields_and_omits_amplification() {
        let facts = DaemonStorageFactsV1::from_exact(None, Some((1024, 0))).unwrap();
        let mut properties = Map::new();
        facts.insert_properties(&mut properties);
        assert_eq!(properties["core_active_logical_bytes_bucket"], "lt_100mb");
        assert_eq!(properties["core_certified_source_bytes_bucket"], "0");
        assert!(!properties.contains_key("core_logical_amplification_bucket"));
        assert!(!properties.contains_key("filesystem_available_to_active_core_ratio_bucket"));
    }

    #[test]
    fn invalid_filesystem_measurement_omits_the_complete_group() {
        assert!(DaemonStorageFactsV1::from_exact(Some((0, 0)), None).is_none());
        assert!(DaemonStorageFactsV1::from_exact(Some((10, 11)), None).is_none());
    }
}
