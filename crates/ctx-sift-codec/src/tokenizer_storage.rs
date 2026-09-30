use rkyv::util::AlignedVec;
use std::sync::LazyLock;

include!(concat!(env!("OUT_DIR"), "/tokenizer_sizes.rs"));

// Both views borrow this immutable owner; the DFA starts at a 16-byte boundary.
static DATA: LazyLock<AlignedVec<16>> = LazyLock::new(|| {
    let mut bytes = AlignedVec::with_capacity(TOTAL_LEN);
    bytes.resize(TOTAL_LEN, 0);
    let mut decoder = zstd::bulk::Decompressor::new().expect("create tokenizer decoder");
    let count_len = decoder
        .decompress_to_buffer(
            include_bytes!(concat!(env!("OUT_DIR"), "/count.rkyv.zst")),
            &mut bytes[..COUNT_LEN],
        )
        .expect("decompress count tables");
    assert_eq!(count_len, COUNT_LEN);
    let pre_len = decoder
        .decompress_to_buffer(
            include_bytes!(concat!(env!("OUT_DIR"), "/pre.dense.zst")),
            &mut bytes[PRE_OFFSET..PRE_OFFSET + PRE_LEN],
        )
        .expect("decompress pre-tokenizer");
    assert_eq!(pre_len, PRE_LEN);
    bytes
});

pub(super) fn count_bytes() -> &'static [u8] {
    // rkyv's root is relative to the end of its own archive, excluding padding.
    &DATA[..COUNT_LEN]
}

pub(super) fn pre_bytes() -> &'static [u8] {
    &DATA[PRE_OFFSET..PRE_OFFSET + PRE_LEN]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_assets_match_raw_inputs_and_share_aligned_storage() {
        let count = count_bytes();
        let pre = pre_bytes();
        assert_eq!(
            count,
            include_bytes!(concat!(env!("OUT_DIR"), "/count.rkyv")).as_slice()
        );
        assert_eq!(
            pre,
            include_bytes!(concat!(env!("OUT_DIR"), "/pre.dense")).as_slice()
        );
        assert_eq!(count.as_ptr().align_offset(16), 0);
        assert_eq!(pre.as_ptr().align_offset(16), 0);
        assert_eq!(pre.as_ptr().addr() - count.as_ptr().addr(), PRE_OFFSET);
        assert_eq!(DATA.len(), TOTAL_LEN);
        assert_eq!(DATA.capacity(), TOTAL_LEN);
        assert!(std::ptr::eq(count, count_bytes()));
        assert!(std::ptr::eq(pre, pre_bytes()));
    }
}
