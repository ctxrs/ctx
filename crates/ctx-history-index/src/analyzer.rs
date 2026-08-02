use tantivy::{
    tokenizer::{LowerCaser, TextAnalyzer, Token, TokenStream, Tokenizer},
    Index,
};

pub(crate) const BODY_ANALYZER: &str = "default";

const MAX_IDENTIFIER_CHARS: usize = 32;
const IDENTIFIER_CHUNK_STRIDE: usize = 24;

#[derive(Clone, Default)]
struct ScriptAwareTokenizer;

struct ScriptAwareTokenStream<'a> {
    text: &'a str,
    cursor: usize,
    long_run: Option<LongRun>,
    token: Token,
}

#[derive(Clone, Copy)]
struct LongRun {
    next_from: usize,
    offset_to: usize,
}

impl Tokenizer for ScriptAwareTokenizer {
    type TokenStream<'a> = ScriptAwareTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        ScriptAwareTokenStream {
            text,
            cursor: 0,
            long_run: None,
            token: Token::default(),
        }
    }
}

impl TokenStream for ScriptAwareTokenStream<'_> {
    fn advance(&mut self) -> bool {
        if self.long_run.is_some() {
            self.emit_long_identifier();
            return true;
        }

        while self.cursor < self.text.len() {
            let offset_from = self.cursor;
            let character = self.text[offset_from..]
                .chars()
                .next()
                .expect("cursor remains on a character boundary");
            let offset_to = offset_from + character.len_utf8();
            if is_dense_script(character) {
                self.cursor = offset_to;
                self.emit(offset_from, offset_to);
                return true;
            }
            if !character.is_alphanumeric() {
                self.cursor = offset_to;
                continue;
            }

            let mut run_to = offset_to;
            let mut character_count = 1;
            for (relative_offset, next) in self.text[offset_to..].char_indices() {
                if !next.is_alphanumeric() || is_dense_script(next) {
                    break;
                }
                run_to = offset_to + relative_offset + next.len_utf8();
                character_count += 1;
            }
            self.cursor = run_to;
            if character_count <= MAX_IDENTIFIER_CHARS {
                self.emit(offset_from, run_to);
            } else {
                self.long_run = Some(LongRun {
                    next_from: offset_from,
                    offset_to: run_to,
                });
                self.emit_long_identifier();
            }
            return true;
        }
        false
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

impl ScriptAwareTokenStream<'_> {
    fn emit_long_identifier(&mut self) {
        let run = self.long_run.expect("long run is present");
        let chunk_to = boundary_after_chars(
            self.text,
            run.next_from,
            run.offset_to,
            MAX_IDENTIFIER_CHARS,
        );
        self.emit(run.next_from, chunk_to);
        self.long_run = if chunk_to == run.offset_to {
            None
        } else {
            Some(LongRun {
                next_from: boundary_after_chars(
                    self.text,
                    run.next_from,
                    run.offset_to,
                    IDENTIFIER_CHUNK_STRIDE,
                ),
                offset_to: run.offset_to,
            })
        };
    }

    fn emit(&mut self, offset_from: usize, offset_to: usize) {
        self.token.offset_from = offset_from;
        self.token.offset_to = offset_to;
        self.token.position = self.token.position.wrapping_add(1);
        self.token.position_length = 1;
        self.token.text.clear();
        self.token.text.push_str(&self.text[offset_from..offset_to]);
    }
}

fn boundary_after_chars(text: &str, from: usize, to: usize, count: usize) -> usize {
    text[from..to]
        .char_indices()
        .nth(count)
        .map_or(to, |(relative, _)| from + relative)
}

fn is_dense_script(character: char) -> bool {
    matches!(
        character as u32,
        0x0E00..=0x0EFF
            | 0x1000..=0x109F
            | 0x1100..=0x11FF
            | 0x1780..=0x17FF
            | 0x3040..=0x30FF
            | 0x3130..=0x318F
            | 0x31F0..=0x31FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
    )
}

pub(crate) fn register_body_analyzer(index: &Index) {
    index.tokenizers().register(BODY_ANALYZER, body_analyzer());
}

pub(crate) fn body_analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(ScriptAwareTokenizer)
        .filter(LowerCaser)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(text: &str) -> Vec<Token> {
        let mut analyzer = TextAnalyzer::builder(ScriptAwareTokenizer)
            .filter(LowerCaser)
            .build();
        let mut stream = analyzer.token_stream(text);
        let mut tokens = Vec::new();
        while stream.advance() {
            tokens.push(stream.token().clone());
        }
        tokens
    }

    #[test]
    fn dense_scripts_and_long_identifiers_emit_bounded_terms() {
        let identifier = "TechnicalIdentifier".repeat(12);
        let expected_dense = ["数", "据", "库", "迁", "移"];
        let analyzed = tokens(&format!("数据库迁移 namespace::{identifier}"));
        assert_eq!(
            analyzed
                .iter()
                .take(expected_dense.len())
                .map(|token| token.text.as_str())
                .collect::<Vec<_>>(),
            expected_dense
        );
        assert!(analyzed
            .iter()
            .all(|token| token.text.chars().count() <= 32));
        assert!(analyzed
            .iter()
            .any(|token| token.text.starts_with("technicalidentifier")));
    }
}
