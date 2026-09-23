use serde::{Deserialize, de::IgnoredAny};
use std::collections::HashMap;

// Pinned bpe 0.2.2 Serde field order. A separate valid count-only representation.
#[derive(Deserialize)]
#[allow(dead_code)]
struct Source<'a> {
    all_tokens: IgnoredAny,
    token_starts: Vec<u32>,
    bytes_hash_to_token: IgnoredAny,
    split_table: Vec<(u32, u32)>,
    pair_lookup: HashMap<(u32, u32), u32>,
    #[serde(borrow)]
    longest_searcher: &'a [u8],
    overlapping_searcher: IgnoredAny,
    overlapping_searcher_rev: IgnoredAny,
    next_prefix_match: Vec<u32>,
    hash_factor: IgnoredAny,
}
fn word(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap())
}
#[path = "src/tokenizer_data.rs"]
mod data;
fn main() {
    let dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let original = &bpe_openai::o200k_base().bpe;
    let serialized = rmp_serde::to_vec(original).unwrap();
    let source: Source = rmp_serde::from_slice(&serialized).unwrap();
    let n = source.split_table.len();
    assert_eq!(n, 199_998);
    assert_eq!(source.token_starts.len(), n + 1);
    assert_eq!(source.next_prefix_match.len(), n);
    let lengths: Vec<_> = source
        .token_starts
        .windows(2)
        .map(|x| x[1].checked_sub(x[0]).filter(|n| *n > 0).unwrap())
        .collect();
    for (id, &(a, b)) in source.split_table.iter().enumerate() {
        assert!((a as usize) < n && (b as usize) < n);
        assert!(
            (a == id as u32 && b == id as u32 && lengths[id] == 1)
                || (a < id as u32
                    && b < id as u32
                    && lengths[a as usize] + lengths[b as usize] == lengths[id])
        );
    }
    for (id, &prefix) in source.next_prefix_match.iter().enumerate() {
        assert!(
            prefix == u32::MAX || ((prefix as usize) < n && lengths[prefix as usize] < lengths[id])
        );
    }
    let mut pairs: Vec<_> = source.pair_lookup.into_iter().collect();
    pairs.sort_unstable_by_key(|&(pair, _)| pair);
    for &((a, b), value) in &pairs {
        assert!((value as usize) < n && a < value && b < value);
        assert_eq!(source.split_table[value as usize], (a, b));
    }
    let data = source.longest_searcher;
    let nstates = word(data) as usize;
    assert_eq!(nstates % 256, 0);
    let states = &data[4..4 + nstates * 12];
    let rest = &data[4 + nstates * 12..];
    let noutputs = word(rest) as usize;
    let outputs = &rest[4..4 + noutputs * 12];
    // Native daachorse trailer is match kind u8 + live-state count u32.
    let trailer = &rest[4 + noutputs * 12..];
    assert_eq!(trailer.len(), 5);
    assert_eq!(trailer[0], 1); // LeftmostLongest
    assert!((word(&trailer[1..]) as usize) <= nstates);
    for row in states.chunks_exact(12) {
        let base = word(row) as usize;
        let fail = word(&row[4..]) as usize;
        let output = (word(&row[8..]) >> 8) as usize;
        assert!(base == 0 || ((base ^ 255) < nstates && base < nstates));
        assert!(fail < nstates && output <= noutputs);
    }
    for row in outputs.chunks_exact(12) {
        let value = word(row) as usize;
        assert!(value < n);
        assert_eq!(word(&row[4..]), lengths[value]);
        assert!((word(&row[8..]) as usize) <= noutputs);
    }
    let data = data::Data {
        lengths: lengths.clone(),
        splits: source.split_table.iter().map(|&(a, b)| [a, b]).collect(),
        prefix: source.next_prefix_match.clone(),
        pairs: pairs
            .iter()
            .map(|&((a, b), v)| ((u64::from(a) << 32) | u64::from(b), v))
            .collect(),
        states: states
            .chunks_exact(12)
            .map(|r| [word(r), word(&r[4..]), word(&r[8..])])
            .collect(),
        outputs: outputs.chunks_exact(12).map(word).collect(),
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&data).unwrap();
    rkyv::access::<data::ArchivedData, rkyv::rancor::Error>(&bytes)
        .expect("valid generated count tables");
    std::fs::write(dir.join("count.rkyv"), bytes).unwrap();
    use regex_automata::dfa::{Automaton, StartKind, dense};
    let dfa = dense::Builder::new()
        .configure(
            dense::Config::new()
                .start_kind(StartKind::Anchored)
                .match_kind(regex_automata::MatchKind::LeftmostFirst)
                .dfa_size_limit(Some(64 * 1024 * 1024))
                .determinize_size_limit(Some(64 * 1024 * 1024)),
        )
        .build_many(&patterns())
        .expect("build o200k pre-tokenizer");
    let target_endian = std::env::var("CARGO_CFG_TARGET_ENDIAN").unwrap();
    let host_endian = if cfg!(target_endian = "little") {
        "little"
    } else {
        "big"
    };
    assert_eq!(
        target_endian, host_endian,
        "pre-tokenizer validation requires matching build-host and target endianness"
    );
    let (bytes, pad) = if target_endian == "little" {
        dfa.to_bytes_little_endian()
    } else {
        dfa.to_bytes_big_endian()
    };
    let serialized = &bytes[pad..];
    let (checked, consumed) =
        dense::DFA::from_bytes(serialized).expect("valid generated pre-tokenizer");
    assert_eq!(consumed, serialized.len());
    assert_eq!(checked.start_kind(), StartKind::Anchored);
    assert_eq!(checked.pattern_len(), 3);
    assert!(checked.is_utf8() && !checked.has_empty());
    std::fs::write(dir.join("pre.dense"), serialized).unwrap();
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/tokenizer_data.rs");
}
fn patterns() -> [String; 3] {
    let pattern=[
            r"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]*[\p{Ll}\p{Lm}\p{Lo}\p{M}]+(?i:'s|'t|'re|'ve|'m|'ll|'d)?",
            r"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]+[\p{Ll}\p{Lm}\p{Lo}\p{M}]*(?i:'s|'t|'re|'ve|'m|'ll|'d)?",
            r"\p{N}{1,3}",r" ?[^\s\p{L}\p{N}]+[\r\n/]*",r"\s*[\r\n]+",r"\s+$",
        ].join("|");
    [pattern, r"\s+\s".into(), r"\s+".into()]
}
