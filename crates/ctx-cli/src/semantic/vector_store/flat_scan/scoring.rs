use super::{validation::validate_norm_squared, FlatScanError, FlatScanInput};

#[derive(Debug, Clone, Copy)]
pub(super) enum ExactDotProductKernel {
    Scalar,
    #[cfg(target_arch = "x86_64")]
    Avx,
}

impl ExactDotProductKernel {
    pub(super) fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx") {
            return Self::Avx;
        }
        Self::Scalar
    }

    #[inline(always)]
    pub(super) fn dot(self, query: &[f32], vector: &[f32]) -> f32 {
        match self {
            Self::Scalar => exact_dot_product_f32_scalar(query, vector),
            #[cfg(target_arch = "x86_64")]
            Self::Avx => {
                // Detection occurs once when the scanner is constructed.
                unsafe { exact_dot_product_f32_avx(query, vector) }
            }
        }
    }
}

/// Eight independent F32 accumulators match the measured flat-F32 prototype.
/// Reduction order is fixed so every backend can use this as its exact oracle.
#[inline(always)]
pub(super) fn exact_dot_product_f32_scalar(query: &[f32], vector: &[f32]) -> f32 {
    let mut sums = [0.0_f32; 8];
    let mut dimension = 0_usize;
    while dimension + sums.len() <= query.len() {
        sums[0] += query[dimension] * vector[dimension];
        sums[1] += query[dimension + 1] * vector[dimension + 1];
        sums[2] += query[dimension + 2] * vector[dimension + 2];
        sums[3] += query[dimension + 3] * vector[dimension + 3];
        sums[4] += query[dimension + 4] * vector[dimension + 4];
        sums[5] += query[dimension + 5] * vector[dimension + 5];
        sums[6] += query[dimension + 6] * vector[dimension + 6];
        sums[7] += query[dimension + 7] * vector[dimension + 7];
        dimension += sums.len();
    }
    let mut similarity = sums.into_iter().sum::<f32>();
    while dimension < query.len() {
        similarity += query[dimension] * vector[dimension];
        dimension += 1;
    }
    similarity
}

/// AVX performs the same multiply, per-lane add, and final scalar lane
/// reduction as `exact_dot_product_f32_scalar`. FMA is deliberately not
/// enabled: contracting the multiply and add would change score bits.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
pub(super) unsafe fn exact_dot_product_f32_avx(query: &[f32], vector: &[f32]) -> f32 {
    use std::arch::x86_64::{
        __m256, _mm256_add_ps, _mm256_loadu_ps, _mm256_mul_ps, _mm256_setzero_ps, _mm256_storeu_ps,
    };

    let mut sums: __m256 = _mm256_setzero_ps();
    let mut dimension = 0_usize;
    while dimension + 8 <= query.len() {
        // The loop bounds prove both unaligned eight-value loads are in range.
        let (query_values, vector_values) = unsafe {
            (
                _mm256_loadu_ps(query.as_ptr().add(dimension)),
                _mm256_loadu_ps(vector.as_ptr().add(dimension)),
            )
        };
        sums = _mm256_add_ps(sums, _mm256_mul_ps(query_values, vector_values));
        dimension += 8;
    }
    let mut lanes = [0.0_f32; 8];
    unsafe {
        _mm256_storeu_ps(lanes.as_mut_ptr(), sums);
    }
    let mut similarity = lanes.into_iter().sum::<f32>();
    while dimension < query.len() {
        similarity += query[dimension] * vector[dimension];
        dimension += 1;
    }
    similarity
}

pub(super) fn validate_and_dot_le_bytes(
    query: &[f32],
    vector: &[u8],
    chunk_ordinal: Option<u32>,
    tolerance: f64,
) -> Result<f32, FlatScanError> {
    let mut norm_squared = 0.0_f64;
    for (dimension, bytes) in vector.chunks_exact(std::mem::size_of::<f32>()).enumerate() {
        let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if !value.is_finite() {
            return Err(FlatScanError::NonFinite {
                input: FlatScanInput::Vector,
                dimension,
                chunk_ordinal,
            });
        }
        norm_squared += f64::from(value) * f64::from(value);
    }
    validate_norm_squared(
        FlatScanInput::Vector,
        chunk_ordinal,
        norm_squared,
        tolerance,
    )?;
    let similarity = exact_dot_product_le_bytes(query, vector);
    if !similarity.is_finite() {
        return Err(FlatScanError::NonFiniteDotProduct { chunk_ordinal });
    }
    Ok(similarity)
}

#[inline(always)]
fn exact_dot_product_le_bytes(query: &[f32], vector: &[u8]) -> f32 {
    let value_at = |dimension: usize| {
        let offset = dimension * std::mem::size_of::<f32>();
        f32::from_le_bytes([
            vector[offset],
            vector[offset + 1],
            vector[offset + 2],
            vector[offset + 3],
        ])
    };
    let mut sums = [0.0_f32; 8];
    let mut dimension = 0_usize;
    while dimension + sums.len() <= query.len() {
        sums[0] += query[dimension] * value_at(dimension);
        sums[1] += query[dimension + 1] * value_at(dimension + 1);
        sums[2] += query[dimension + 2] * value_at(dimension + 2);
        sums[3] += query[dimension + 3] * value_at(dimension + 3);
        sums[4] += query[dimension + 4] * value_at(dimension + 4);
        sums[5] += query[dimension + 5] * value_at(dimension + 5);
        sums[6] += query[dimension + 6] * value_at(dimension + 6);
        sums[7] += query[dimension + 7] * value_at(dimension + 7);
        dimension += sums.len();
    }
    let mut similarity = sums.into_iter().sum::<f32>();
    while dimension < query.len() {
        similarity += query[dimension] * value_at(dimension);
        dimension += 1;
    }
    similarity
}
