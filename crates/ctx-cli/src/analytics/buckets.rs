use std::time::Duration;

use crate::progress::ProgressArg;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CountBucket {
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

pub(crate) fn count_bucket(count: u64) -> CountBucket {
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
pub(crate) enum BytesBucket {
    Zero,
    UnderOneHundredKb,
    OneHundredKbToOneMb,
    OneToTenMb,
    TenToOneHundredMb,
    OneHundredMbToOneGb,
    OneToTenGb,
    TenToOneHundredGb,
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
            Self::OneToTenGb => "1gb-10gb",
            Self::TenToOneHundredGb => "10gb-100gb",
            Self::OverOneHundredGb => "100gb+",
        }
    }
}

pub(crate) fn bytes_bucket(bytes: u64) -> BytesBucket {
    match bytes {
        0 => BytesBucket::Zero,
        1..=102_399 => BytesBucket::UnderOneHundredKb,
        102_400..=1_048_575 => BytesBucket::OneHundredKbToOneMb,
        1_048_576..=10_485_759 => BytesBucket::OneToTenMb,
        10_485_760..=104_857_599 => BytesBucket::TenToOneHundredMb,
        104_857_600..=1_073_741_823 => BytesBucket::OneHundredMbToOneGb,
        1_073_741_824..=10_737_418_239 => BytesBucket::OneToTenGb,
        10_737_418_240..=107_374_182_399 => BytesBucket::TenToOneHundredGb,
        _ => BytesBucket::OverOneHundredGb,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextLengthBucket {
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

pub(crate) fn text_length_bucket(chars: usize) -> TextLengthBucket {
    match chars {
        0 => TextLengthBucket::Zero,
        1..=20 => TextLengthBucket::OneToTwenty,
        21..=100 => TextLengthBucket::TwentyOneToOneHundred,
        101..=500 => TextLengthBucket::OneHundredOneToFiveHundred,
        _ => TextLengthBucket::OverFiveHundred,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurationBucket {
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

pub(crate) fn duration_bucket(duration: Duration) -> DurationBucket {
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
pub(crate) enum ProgressMode {
    Auto,
    Plain,
    Json,
    None,
}

impl ProgressMode {
    pub(super) fn from_arg(value: ProgressArg) -> Self {
        match value {
            ProgressArg::Auto => Self::Auto,
            ProgressArg::Plain => Self::Plain,
            ProgressArg::Json => Self::Json,
            ProgressArg::None => Self::None,
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Plain => "plain",
            Self::Json => "json",
            Self::None => "none",
        }
    }
}
