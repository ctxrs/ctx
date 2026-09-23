//! Offline, reversible compaction selected by ordinary o200k_base token counts.

mod json_codec;
mod json_length;
mod text_codec;
mod text_lines;
mod text_refs;
mod text_symbols;
mod tokenizer;

use serde::{Deserialize, Serialize};

fn strip_product_header<'a>(input: &'a str, current: &str) -> Option<&'a str> {
    const PRIOR_PRODUCT: &[u8] = &[114, 101, 116, 111, 107];
    let (current_product, suffix) = current.split_once(':')?;
    let (product, body) = input.split_once(':')?;
    (product == current_product || product.as_bytes() == PRIOR_PRODUCT)
        .then(|| body.strip_prefix(suffix))?
}

/// The explicit restoration format; raw text is never interpreted as framing.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum Encoding {
    #[serde(rename = "raw")]
    Raw,
    #[serde(rename = "json-v1")]
    JsonV1,
    #[serde(rename = "json-rows-v1")]
    JsonRowsV1,
    #[serde(rename = "json-min-v1")]
    JsonMinV1,
    #[serde(rename = "json-columns-v1")]
    JsonColumnsV1,
    #[serde(rename = "text-runs-v1")]
    TextRunsV1,
    #[serde(rename = "text-prefixes-v1")]
    TextPrefixesV1,
    #[serde(rename = "text-refs-v1")]
    TextRefsV1,
    #[serde(rename = "text-lines-v1")]
    TextLinesV1,
    #[serde(rename = "text-symbols-v1")]
    TextSymbolsV1,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompactResult {
    pub text: String,
    pub encoding: Encoding,
    pub input_tokens: usize,
    pub output_tokens: usize,
}

/// Instances share the embedded ordinary o200k tokenizer within the process.
pub struct Compactor {
    tokenizer: &'static tokenizer::CountTokenizer,
}

impl Compactor {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            tokenizer: tokenizer::o200k_base(),
        })
    }

    /// Count one complete text using the same ordinary-token policy as compaction.
    pub fn count_tokens(&self, text: &str) -> usize {
        self.tokenizer.count(text)
    }

    /// Choose only strictly cheaper complete representations. Ties favor raw,
    /// then the first codec candidate. Special-token-looking text is ordinary text.
    pub fn compact(&self, text: &str) -> CompactResult {
        let input_tokens = self.count_tokens(text);
        let mut selected = None;
        let mut encoding = Encoding::Raw;
        let mut output_tokens = input_tokens;
        let mut priority = (false, 0usize);
        let mut consider = |candidate_encoding, candidate: String, candidate_priority| {
            // An earlier format may tie an already counted symbol. Count that
            // equality exactly before restoring the original format priority.
            let limit = output_tokens + usize::from(candidate_priority < priority);
            if let Some(tokens) = self.tokenizer.count_below(&candidate, limit) {
                selected = Some(candidate);
                encoding = candidate_encoding;
                output_tokens = tokens;
                priority = candidate_priority;
            }
        };
        let candidates = json_codec::candidates(text)
            .into_iter()
            .chain(
                std::iter::once_with(|| {
                    text_codec::candidate(text).map(|candidate| (Encoding::TextRunsV1, candidate))
                })
                .flatten(),
            )
            .chain(
                std::iter::once_with(|| {
                    text_codec::prefix_candidate(text)
                        .map(|candidate| (Encoding::TextPrefixesV1, candidate))
                })
                .flatten(),
            );
        let mut index = 1;
        for (candidate_encoding, candidate) in candidates {
            consider(candidate_encoding, candidate, (false, index));
            index += 1;
        }
        for plan in text_refs::candidates(text) {
            if let Some(symbols) =
                text_symbols::candidate(&plan, |literal| self.count_tokens(literal))
            {
                consider(Encoding::TextSymbolsV1, symbols, (true, index));
            }
            consider(Encoding::TextRefsV1, plan.into_references(), (false, index));
            index += 1;
        }
        if let Some(candidate) = text_lines::candidate(text) {
            consider(Encoding::TextLinesV1, candidate, (false, index));
        }
        // All old symbols have an earlier index; all other old formats sort
        // before symbols. An equal-cost comma proposal therefore never wins.
        if let Some(plan) = text_refs::comma_candidate(text)
            && let Some(symbols) =
                text_symbols::candidate(&plan, |literal| self.count_tokens(literal))
        {
            consider(Encoding::TextSymbolsV1, symbols, (true, index));
        }
        CompactResult {
            text: selected.unwrap_or_else(|| text.to_owned()),
            encoding,
            input_tokens,
            output_tokens,
        }
    }
}

/// Restore a representation using its explicitly supplied format.
pub fn restore(encoding: Encoding, text: &str) -> anyhow::Result<String> {
    match encoding {
        Encoding::Raw => Ok(text.to_owned()),
        Encoding::TextRunsV1 => text_codec::restore(text),
        Encoding::TextPrefixesV1 => text_codec::restore_prefixes(text),
        Encoding::TextRefsV1 => text_refs::restore(text),
        Encoding::TextLinesV1 => text_lines::restore(text),
        Encoding::TextSymbolsV1 => text_symbols::restore(text),
        Encoding::JsonV1 | Encoding::JsonRowsV1 | Encoding::JsonMinV1 | Encoding::JsonColumnsV1 => {
            json_codec::restore(encoding, text)
        }
    }
}
