use super::{FlatScanConfig, FlatScanError, FlatScanInput};

pub(super) fn validate_config(config: FlatScanConfig) -> Result<usize, FlatScanError> {
    if config.dimensions == 0 {
        return Err(FlatScanError::ZeroDimensions);
    }
    if !config.normalization_tolerance.is_finite()
        || !(0.0..1.0).contains(&config.normalization_tolerance)
    {
        return Err(FlatScanError::InvalidNormalizationTolerance {
            tolerance: config.normalization_tolerance,
        });
    }
    config
        .dimensions
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or(FlatScanError::DimensionByteSizeOverflow {
            dimensions: config.dimensions,
        })
}

pub(super) fn validate_normalized_f32(
    values: &[f32],
    input: FlatScanInput,
    chunk_ordinal: Option<u32>,
    tolerance: f64,
) -> Result<(), FlatScanError> {
    let mut norm_squared = 0.0_f64;
    for (dimension, value) in values.iter().copied().enumerate() {
        if !value.is_finite() {
            return Err(FlatScanError::NonFinite {
                input,
                dimension,
                chunk_ordinal,
            });
        }
        norm_squared += f64::from(value) * f64::from(value);
    }
    validate_norm_squared(input, chunk_ordinal, norm_squared, tolerance)
}

pub(super) fn validate_norm_squared(
    input: FlatScanInput,
    chunk_ordinal: Option<u32>,
    norm_squared: f64,
    tolerance: f64,
) -> Result<(), FlatScanError> {
    if norm_squared == 0.0 {
        return Err(FlatScanError::ZeroNorm {
            input,
            chunk_ordinal,
        });
    }
    if !norm_squared.is_finite() || (norm_squared - 1.0).abs() > tolerance {
        return Err(FlatScanError::NotNormalized {
            input,
            norm_squared,
            tolerance,
            chunk_ordinal,
        });
    }
    Ok(())
}
