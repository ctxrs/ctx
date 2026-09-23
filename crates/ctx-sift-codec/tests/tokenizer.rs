use sift::Compactor;

mod candidate {
    include!("../src/tokenizer.rs");

    // Inspect actual token IDs without adding a decoding/encoding API to the
    // production count-only type. Public Compactor counts are checked too.
    pub fn encode(text: &str) -> Vec<u32> {
        let tokenizer = o200k_base();
        tokenizer
            .pieces(text)
            .flat_map(|piece| tokenizer.tables.encode(piece.as_bytes()))
            .collect()
    }
}

fn assert_encoding(text: &str, reference: &tiktoken_rs::CoreBPE, compactor: &Compactor) {
    let expected = reference.encode_ordinary(text);
    assert_eq!(
        candidate::encode(text),
        expected,
        "embedded encoding differs for {:?}",
        text.chars().take(100).collect::<String>()
    );
    assert_eq!(
        bpe_openai::o200k_base().encode(text),
        expected,
        "ordinary encoding differs for {:?}",
        text.chars().take(100).collect::<String>()
    );
    assert_eq!(compactor.count_tokens(text), expected.len());
    assert_eq!(candidate::o200k_base().count(text), expected.len());
}

#[test]
fn complete_vocabulary_matches_reference_bytes_and_ordinary_encoding() {
    let reference = tiktoken_rs::o200k_base().unwrap();
    let tokenizer = bpe_openai::o200k_base();
    let compactor = Compactor::new().unwrap();
    assert_eq!(tokenizer.bpe.num_tokens(), 199_998);
    for rank in 0..199_998 {
        let bytes = reference.decode_bytes(&[rank]).unwrap();
        assert_eq!(tokenizer.bpe.token_bytes(rank), bytes, "rank {rank}");
        // Individual tokens need not be UTF-8; compare their bytes above and
        // exercise their lossy UTF-8 rendering as ordinary text as well.
        assert_encoding(&String::from_utf8_lossy(&bytes), &reference, &compactor);
    }
}

#[test]
fn unicode_whitespace_special_looking_and_long_pieces_are_ordinary() {
    let reference = tiktoken_rs::o200k_base().unwrap();
    let compactor = Compactor::new().unwrap();
    let cases = [
        String::new(),
        "<|endoftext|><|endofprompt|><|fim_prefix|><|start|><|return|>".into(),
        "I'm I'M we've WE'VE they're THEY'RE e\u{301} é 中文 العربية हिन्दी 🦀👨‍👩‍👧‍👦\0".into(),
        "a".repeat(200_000),
        "AbCdEFgh".repeat(25_000),
        "🦀".repeat(10_000),
        " \t\u{a0}\u{2003}".repeat(10_000),
        "?!-/".repeat(25_000),
    ];
    for text in cases {
        assert_encoding(&text, &reference, &compactor);
    }
    // Cover greedy whitespace lookahead at EOF, before words, and near newlines.
    let spaces = [
        " ", "\t", "\r", "\n", "\r\n", "\u{b}", "\u{c}", "\u{85}", "\u{a0}", "\u{2003}",
        "\u{2028}", "\u{2029}", "\u{3000}",
    ];
    for a in spaces {
        for b in spaces {
            for c in spaces {
                for suffix in ["", "word", "字", "!", "\n"] {
                    assert_encoding(&format!("x{a}{b}{c}{suffix}"), &reference, &compactor);
                }
            }
        }
    }
    // Every Unicode scalar, in bounded inputs with both case and combining-mark
    // neighbors. This catches differences in the pre-tokenizer's Unicode classes.
    let mut text = String::new();
    for scalar in 0..=0x10ffff {
        if let Some(ch) = char::from_u32(scalar) {
            text.push('a');
            text.push(ch);
            text.push_str("A\u{301}\n");
        }
        if scalar % 1024 == 1023 {
            assert_encoding(&text, &reference, &compactor);
            text.clear();
        }
    }
}

#[test]
fn synthetic_mixed_token_sequences_match_without_chunking() {
    let reference = tiktoken_rs::o200k_base().unwrap();
    let compactor = Compactor::new().unwrap();
    let mut state = 0x5124_97ab_cdef_0123u64;
    for _ in 0..1024 {
        let mut bytes = Vec::new();
        for _ in 0..64 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            bytes.extend(reference.decode_bytes(&[(state % 199_998) as u32]).unwrap());
        }
        assert_encoding(&String::from_utf8_lossy(&bytes), &reference, &compactor);
    }
}

#[test]
fn bounded_candidate_counts_preserve_context_boundaries_and_strict_limits() {
    let reference = tiktoken_rs::o200k_base().unwrap();
    let tokenizer = candidate::o200k_base();
    let original =
        " repeated pieces aren't chunks: café 字 🦀\r\n\tword! <|endoftext|>\n".repeat(80);
    assert_eq!(
        tokenizer.count(&original),
        reference.encode_ordinary(&original).len()
    );
    // Candidate boundaries must come from its complete original pre-tokenizer.
    for text in [
        "",
        "word",
        "wordword",
        " wordword",
        "word\r\n word",
        "aren'taren't",
        "café café字",
        "🦀\u{301}字!",
        "<|endoftext|>word",
        "\n\n\t word ",
        "prefix never present in original; new pieces still need exact BPE",
        &original,
    ] {
        let expected = reference.encode_ordinary(text).len();
        assert_eq!(tokenizer.count(text), expected);
        for limit in [0, expected.saturating_sub(1), expected, expected + 1] {
            assert_eq!(
                tokenizer.count_below(text, limit),
                (expected < limit).then_some(expected)
            );
        }
    }
}
