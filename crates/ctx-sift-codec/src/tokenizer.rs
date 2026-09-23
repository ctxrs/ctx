// Count-only storage adaptation of bpe 0.2.2 / bpe-openai 0.3.1 (MIT),
// copyright 2023 GitHub Inc., and aneubeck-daachorse 1.1.1 (MIT).
// See THIRD_PARTY_TOKENIZER.md for complete notices and pinned source identities.
use regex_automata::{
    Anchored, Input,
    dfa::{Automaton, dense::DFA},
};
#[path = "tokenizer_data.rs"]
mod data;
#[repr(C, align(16))]
struct Aligned<const N: usize>([u8; N]);
static COUNT_DATA: Aligned<{ include_bytes!(concat!(env!("OUT_DIR"), "/count.rkyv")).len() }> =
    Aligned(*include_bytes!(concat!(env!("OUT_DIR"), "/count.rkyv")));
static PRE_DATA: Aligned<{ include_bytes!(concat!(env!("OUT_DIR"), "/pre.dense")).len() }> =
    Aligned(*include_bytes!(concat!(env!("OUT_DIR"), "/pre.dense")));
struct CountTables {
    data: &'static data::ArchivedData,
}
impl CountTables {
    fn new() -> Self {
        Self {
            // SAFETY: build.rs validates these exact bytes with the shared schema
            // and pinned LE/aligned/32-bit rkyv format. This immutable static is
            // 16-byte aligned, covering every archived type in the schema. The
            // complete array preserves the validated offsets and root position.
            data: unsafe { rkyv::access_unchecked::<data::ArchivedData>(&COUNT_DATA.0) },
        }
    }
    fn token_len(&self, id: u32) -> usize {
        self.data.lengths[id as usize].to_native() as usize
    }
    fn next_prefix(&self, id: u32) -> Option<u32> {
        let next = self.data.prefix[id as usize].to_native();
        (next != u32::MAX).then_some(next)
    }
    fn pair(&self, a: u32, b: u32) -> Option<u32> {
        let key = (u64::from(a) << 32) | u64::from(b);
        self.data
            .pairs
            .get_with(&key, |k, v| *k == v.to_native())
            .map(|v| v.to_native())
    }
    fn split(&self, id: u32) -> (u32, u32) {
        let r = &self.data.splits[id as usize];
        (r[0].to_native(), r[1].to_native())
    }
    fn state(&self, id: u32) -> (u32, u32, u32) {
        let r = &self.data.states[id as usize];
        (r[0].to_native(), r[1].to_native(), r[2].to_native())
    }
    fn output(&self, id: u32) -> u32 {
        self.data.outputs[id as usize].to_native()
    }
    fn is_valid_token_pair(&self, mut token1: u32, mut token2: u32) -> bool {
        let mut limit = u32::MAX;
        loop {
            if let Some(combined) = self.pair(token1, token2)
                && combined < limit
            {
                return false;
            }
            if token1 > token2 {
                limit = token1;
                token1 = self.split(token1).1;
                if token1 == limit {
                    limit = token2 + 1;
                    token2 = self.split(token2).0;
                    if token2 + 1 == limit {
                        return true;
                    }
                }
            } else {
                limit = token2 + 1;
                token2 = self.split(token2).0;
                if token2 + 1 == limit {
                    limit = token1;
                    token1 = self.split(token1).1;
                    if token1 == limit {
                        return true;
                    }
                }
            }
        }
    }
    fn next_state(&self, mut state: u32, c: u8) -> u32 {
        loop {
            let (base, fail, _) = self.state(state);
            if base != 0 {
                let child = base ^ u32::from(c);
                if self.state(child).2 as u8 == c {
                    return child;
                }
            }
            if state == 0 || fail == 1 {
                return 0;
            }
            state = fail;
        }
    }
    fn next_match(&self, text: &[u8]) -> Option<u32> {
        let mut state = 0;
        let mut last_output = 0;
        for &c in text {
            state = self.next_state(state, c);
            if state == 0 {
                if last_output != 0 {
                    return Some(self.output(last_output - 1));
                }
            } else {
                let output = self.state(state).2 >> 8;
                if output != 0 {
                    last_output = output;
                }
            }
        }
        (last_output != 0).then(|| self.output(last_output - 1))
    }
    fn count(&self, text: &[u8], scratch: &mut EncodeScratch) -> usize {
        let Some(first) = self.next_match(text) else {
            return 0;
        };
        // The initial step accepts this match: no previous token, all bits set.
        if self.token_len(first) == text.len() {
            return 1;
        }
        let mut encoder = BacktrackEncoder::new(self, text, Some(first), scratch);
        while encoder.step().is_some() {}
        encoder.tokens.len()
    }
    #[cfg(test)]
    #[allow(dead_code)] // Token IDs are inspected by the integration test module.
    fn encode(&self, text: &[u8]) -> Vec<u32> {
        let mut scratch = EncodeScratch::default();
        {
            let mut encoder =
                BacktrackEncoder::new(self, text, self.next_match(text), &mut scratch);
            while encoder.step().is_some() {}
        }
        scratch.tokens
    }
}

#[derive(Default)]
struct EncodeScratch {
    tokens: Vec<u32>,
    bitfield: Vec<u64>,
}

impl EncodeScratch {
    fn prepare(&mut self, bytes: usize) {
        self.tokens.clear();
        self.bitfield.resize((bytes + 1).div_ceil(64), u64::MAX);
        self.bitfield.fill(u64::MAX);
    }
}

// bpe 0.2.2 BacktrackEncoder: same stepping/backtracking and bitset algorithm.
struct BacktrackEncoder<'a, 's> {
    bpe: &'a CountTables,
    text: &'a [u8],
    tokens: &'s mut Vec<u32>,
    next_token: Option<u32>,
    pos: usize,
    bitfield: &'s mut Vec<u64>,
}
impl<'a, 's> BacktrackEncoder<'a, 's> {
    fn new(
        bpe: &'a CountTables,
        text: &'a [u8],
        next_token: Option<u32>,
        scratch: &'s mut EncodeScratch,
    ) -> Self {
        scratch.prepare(text.len());
        Self {
            bpe,
            text,
            tokens: &mut scratch.tokens,
            next_token,
            pos: 0,
            bitfield: &mut scratch.bitfield,
        }
    }
    fn step(&mut self) -> Option<u32> {
        let mut token = self.next_token?;
        let last = self.tokens.last().copied();
        loop {
            let token_len = self.bpe.token_len(token);
            let end_pos = self.pos + token_len;
            if self.bitfield[end_pos / 64] & (1 << (end_pos % 64)) != 0
                && last
                    .map(|last_token| self.bpe.is_valid_token_pair(last_token, token))
                    .unwrap_or(true)
            {
                self.tokens.push(token);
                self.pos = end_pos;
                self.next_token = self.bpe.next_match(&self.text[end_pos..]);
                break;
            } else if let Some(shorter) = self.bpe.next_prefix(token) {
                token = shorter;
            } else {
                self.bitfield[self.pos / 64] &= !(1 << (self.pos % 64));
                self.tokens.pop();
                self.pos -= last.map(|t| self.bpe.token_len(t)).unwrap_or(0);
                self.next_token = last;
                break;
            }
        }
        self.next_token
    }
}

pub(crate) struct CountTokenizer {
    pre: DFA<&'static [u32]>,
    tables: CountTables,
}
impl CountTokenizer {
    pub(crate) fn new() -> Self {
        Self {
            tables: CountTables::new(),
            // SAFETY: build.rs validates these exact, padding-stripped bytes
            // with the same pinned DFA format and matching host/target endian.
            // PRE_DATA is immutable and aligned beyond the required u32 boundary.
            // The anchored, nonempty UTF-8 patterns and their order are unchanged.
            pre: unsafe { DFA::from_bytes_unchecked(&PRE_DATA.0) }
                .expect("valid embedded pre-tokenizer")
                .0,
        }
    }
    fn pieces<'a>(&'a self, text: &'a str) -> impl Iterator<Item = &'a str> {
        let mut last = 0;
        std::iter::from_fn(move || {
            let input = Input::new(&text[last..]).anchored(Anchored::Yes);
            let m = self.pre.try_search_fwd(&input).unwrap()?;
            let start = last;
            let mut end = last + m.offset();
            if m.pattern().as_usize() == 1 {
                end -= text[start..end].chars().next_back().unwrap().len_utf8();
                assert_ne!(end, start);
            }
            last = end;
            Some(&text[start..end])
        })
    }
    pub(crate) fn count(&self, text: &str) -> usize {
        let mut scratch = EncodeScratch::default();
        self.pieces(text)
            .map(|p| self.tables.count(p.as_bytes(), &mut scratch))
            .sum()
    }

    /// A partial count is only a rejection, never an emitted token measurement.
    /// BPE is completed for each exact pre-tokenizer piece before testing the limit.
    pub(crate) fn count_below(&self, text: &str, limit: usize) -> Option<usize> {
        if limit == 0 {
            return None;
        }
        let mut total = 0;
        let mut scratch = EncodeScratch::default();
        for piece in self.pieces(text) {
            total += self.tables.count(piece.as_bytes(), &mut scratch);
            if total >= limit {
                return None;
            }
        }
        Some(total)
    }
}
pub(crate) fn o200k_base() -> &'static CountTokenizer {
    static TOKENIZER: std::sync::LazyLock<CountTokenizer> =
        std::sync::LazyLock::new(CountTokenizer::new);
    &TOKENIZER
}

#[cfg(test)]
#[test]
fn embedded_count_archive_is_valid() {
    let checked = rkyv::access::<data::ArchivedData, rkyv::rancor::Error>(&COUNT_DATA.0)
        .expect("valid embedded count tables");
    assert!(std::ptr::eq(checked, CountTables::new().data));
}

#[cfg(test)]
#[test]
fn embedded_pre_tokenizer_is_valid() {
    let (checked, consumed) = DFA::from_bytes(&PRE_DATA.0).unwrap();
    assert_eq!(consumed, PRE_DATA.0.len());
    assert_eq!(
        checked.start_kind(),
        regex_automata::dfa::StartKind::Anchored
    );
    assert_eq!(checked.pattern_len(), 3);
    assert!(checked.is_utf8() && !checked.has_empty());
    let runtime = CountTokenizer::new();
    for (text, expected) in [
        ("abc", Some((0, 3))),
        (" \u{2003}x", Some((1, 4))),
        ("", None),
    ] {
        let input = Input::new(text).anchored(Anchored::Yes);
        for dfa in [&checked, &runtime.pre] {
            let found = dfa.try_search_fwd(&input).unwrap();
            assert_eq!(
                found.map(|m| (m.pattern().as_usize(), m.offset())),
                expected
            );
        }
    }
}
