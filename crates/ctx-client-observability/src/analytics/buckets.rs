use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountBucket {
    Zero,
    One,
    TwoToFive,
    SixToTwenty,
    TwentyOneToOneHundred,
    OneHundredOneToOneThousand,
    OneThousandOneToTenThousand,
    TenThousandOneToOneHundredThousand,
    OneHundredThousandOneToOneMillion,
    OverOneMillion,
}

impl CountBucket {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Zero => "0",
            Self::One => "1",
            Self::TwoToFive => "2-5",
            Self::SixToTwenty => "6-20",
            Self::TwentyOneToOneHundred => "21-100",
            Self::OneHundredOneToOneThousand => "101-1k",
            Self::OneThousandOneToTenThousand => "1k-10k",
            Self::TenThousandOneToOneHundredThousand => "10k-100k",
            Self::OneHundredThousandOneToOneMillion => "100k-1m",
            Self::OverOneMillion => "1m+",
        }
    }
}

pub fn count_bucket(count: u64) -> CountBucket {
    match count {
        0 => CountBucket::Zero,
        1 => CountBucket::One,
        2..=5 => CountBucket::TwoToFive,
        6..=20 => CountBucket::SixToTwenty,
        21..=100 => CountBucket::TwentyOneToOneHundred,
        101..=1_000 => CountBucket::OneHundredOneToOneThousand,
        1_001..=10_000 => CountBucket::OneThousandOneToTenThousand,
        10_001..=100_000 => CountBucket::TenThousandOneToOneHundredThousand,
        100_001..=1_000_000 => CountBucket::OneHundredThousandOneToOneMillion,
        _ => CountBucket::OverOneMillion,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BytesBucket {
    Zero,
    UnderOneHundredKb,
    OneHundredKbToOneMb,
    OneToTenMb,
    TenToOneHundredMb,
    OneHundredMbToOneGb,
    OneToTwoGb,
    TwoToFiveGb,
    FiveToTenGb,
    TenToTwentyFiveGb,
    TwentyFiveToFiftyGb,
    FiftyToOneHundredGb,
    OverOneHundredGb,
}

impl BytesBucket {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Zero => "0",
            Self::UnderOneHundredKb => "lt_100kb",
            Self::OneHundredKbToOneMb => "100kb-1mb",
            Self::OneToTenMb => "1mb-10mb",
            Self::TenToOneHundredMb => "10mb-100mb",
            Self::OneHundredMbToOneGb => "100mb-1gb",
            Self::OneToTwoGb => "1gb-2gb",
            Self::TwoToFiveGb => "2gb-5gb",
            Self::FiveToTenGb => "5gb-10gb",
            Self::TenToTwentyFiveGb => "10gb-25gb",
            Self::TwentyFiveToFiftyGb => "25gb-50gb",
            Self::FiftyToOneHundredGb => "50gb-100gb",
            Self::OverOneHundredGb => "100gb+",
        }
    }
}

pub fn bytes_bucket(bytes: u64) -> BytesBucket {
    match bytes {
        0 => BytesBucket::Zero,
        1..=102_399 => BytesBucket::UnderOneHundredKb,
        102_400..=1_048_575 => BytesBucket::OneHundredKbToOneMb,
        1_048_576..=10_485_759 => BytesBucket::OneToTenMb,
        10_485_760..=104_857_599 => BytesBucket::TenToOneHundredMb,
        104_857_600..=1_073_741_823 => BytesBucket::OneHundredMbToOneGb,
        1_073_741_824..=2_147_483_647 => BytesBucket::OneToTwoGb,
        2_147_483_648..=5_368_709_119 => BytesBucket::TwoToFiveGb,
        5_368_709_120..=10_737_418_239 => BytesBucket::FiveToTenGb,
        10_737_418_240..=26_843_545_599 => BytesBucket::TenToTwentyFiveGb,
        26_843_545_600..=53_687_091_199 => BytesBucket::TwentyFiveToFiftyGb,
        53_687_091_200..=107_374_182_399 => BytesBucket::FiftyToOneHundredGb,
        _ => BytesBucket::OverOneHundredGb,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBytesBucket {
    Zero,
    UnderOneHundredMb,
    OneHundredMbToOneGb,
    OneToFiveGb,
    FiveToTenGb,
    TenToTwentyFiveGb,
    TwentyFiveToFiftyGb,
    FiftyToOneHundredGb,
    OneHundredToTwoHundredFiftyGb,
    TwoHundredFiftyToFiveHundredGb,
    FiveHundredGbToOneTb,
    OneToTwoTb,
    TwoToFiveTb,
    AtLeastFiveTb,
}

impl StorageBytesBucket {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Zero => "0",
            Self::UnderOneHundredMb => "lt_100mb",
            Self::OneHundredMbToOneGb => "100mb-1gb",
            Self::OneToFiveGb => "1gb-5gb",
            Self::FiveToTenGb => "5gb-10gb",
            Self::TenToTwentyFiveGb => "10gb-25gb",
            Self::TwentyFiveToFiftyGb => "25gb-50gb",
            Self::FiftyToOneHundredGb => "50gb-100gb",
            Self::OneHundredToTwoHundredFiftyGb => "100gb-250gb",
            Self::TwoHundredFiftyToFiveHundredGb => "250gb-500gb",
            Self::FiveHundredGbToOneTb => "500gb-1tb",
            Self::OneToTwoTb => "1tb-2tb",
            Self::TwoToFiveTb => "2tb-5tb",
            Self::AtLeastFiveTb => "5tb+",
        }
    }
}

pub fn storage_bytes_bucket(bytes: u64) -> StorageBytesBucket {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    const TIB: u64 = 1024 * GIB;

    if bytes == 0 {
        StorageBytesBucket::Zero
    } else if bytes < 100 * MIB {
        StorageBytesBucket::UnderOneHundredMb
    } else if bytes < GIB {
        StorageBytesBucket::OneHundredMbToOneGb
    } else if bytes < 5 * GIB {
        StorageBytesBucket::OneToFiveGb
    } else if bytes < 10 * GIB {
        StorageBytesBucket::FiveToTenGb
    } else if bytes < 25 * GIB {
        StorageBytesBucket::TenToTwentyFiveGb
    } else if bytes < 50 * GIB {
        StorageBytesBucket::TwentyFiveToFiftyGb
    } else if bytes < 100 * GIB {
        StorageBytesBucket::FiftyToOneHundredGb
    } else if bytes < 250 * GIB {
        StorageBytesBucket::OneHundredToTwoHundredFiftyGb
    } else if bytes < 500 * GIB {
        StorageBytesBucket::TwoHundredFiftyToFiveHundredGb
    } else if bytes < TIB {
        StorageBytesBucket::FiveHundredGbToOneTb
    } else if bytes < 2 * TIB {
        StorageBytesBucket::OneToTwoTb
    } else if bytes < 5 * TIB {
        StorageBytesBucket::TwoToFiveTb
    } else {
        StorageBytesBucket::AtLeastFiveTb
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextLengthBucket {
    Zero,
    OneToTwenty,
    TwentyOneToOneHundred,
    OneHundredOneToFiveHundred,
    OverFiveHundred,
}

impl TextLengthBucket {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Zero => "0",
            Self::OneToTwenty => "1-20",
            Self::TwentyOneToOneHundred => "21-100",
            Self::OneHundredOneToFiveHundred => "101-500",
            Self::OverFiveHundred => "500+",
        }
    }
}

pub fn text_length_bucket(chars: usize) -> TextLengthBucket {
    match chars {
        0 => TextLengthBucket::Zero,
        1..=20 => TextLengthBucket::OneToTwenty,
        21..=100 => TextLengthBucket::TwentyOneToOneHundred,
        101..=500 => TextLengthBucket::OneHundredOneToFiveHundred,
        _ => TextLengthBucket::OverFiveHundred,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationBucket {
    Unknown,
    UnderOneHundredMs,
    UnderOneSecond,
    UnderFiveSeconds,
    UnderThirtySeconds,
    UnderTwoMinutes,
    UnderTenMinutes,
    UnderOneHour,
    AtLeastOneHour,
}

impl DurationBucket {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::UnderOneHundredMs => "lt_100ms",
            Self::UnderOneSecond => "lt_1s",
            Self::UnderFiveSeconds => "lt_5s",
            Self::UnderThirtySeconds => "lt_30s",
            Self::UnderTwoMinutes => "lt_2m",
            Self::UnderTenMinutes => "lt_10m",
            Self::UnderOneHour => "lt_1h",
            Self::AtLeastOneHour => "gte_1h",
        }
    }
}

pub fn duration_bucket(duration: Duration) -> DurationBucket {
    match duration.as_millis() {
        0..=99 => DurationBucket::UnderOneHundredMs,
        100..=999 => DurationBucket::UnderOneSecond,
        1_000..=4_999 => DurationBucket::UnderFiveSeconds,
        5_000..=29_999 => DurationBucket::UnderThirtySeconds,
        30_000..=119_999 => DurationBucket::UnderTwoMinutes,
        120_000..=599_999 => DurationBucket::UnderTenMinutes,
        600_000..=3_599_999 => DurationBucket::UnderOneHour,
        _ => DurationBucket::AtLeastOneHour,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    Auto,
    Plain,
    Json,
    None,
}

impl ProgressMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Plain => "plain",
            Self::Json => "json",
            Self::None => "none",
        }
    }
}
