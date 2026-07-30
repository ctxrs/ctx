use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Primitive {
    PrintMacro,
    DirectWrite,
    StdoutConstructor,
    StderrConstructor,
    OutputRawHelper,
    UiRawWriter,
    UiWriterInjection,
    DocumentRender,
    ClapParse,
}

impl Primitive {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PrintMacro => "print_macro",
            Self::DirectWrite => "direct_write",
            Self::StdoutConstructor => "stdout_constructor",
            Self::StderrConstructor => "stderr_constructor",
            Self::OutputRawHelper => "output_raw_helper",
            Self::UiRawWriter => "ui_raw_writer",
            Self::UiWriterInjection => "ui_writer_injection",
            Self::DocumentRender => "document_render",
            Self::ClapParse => "clap_parse",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputClass {
    MachineProtocol,
    JustifiedPlainHuman,
    Infrastructure,
    CapabilityProbe,
    Violation,
}

impl OutputClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::MachineProtocol => "machine_protocol",
            Self::JustifiedPlainHuman => "justified_plain_human",
            Self::Infrastructure => "infrastructure",
            Self::CapabilityProbe => "capability_probe",
            Self::Violation => "violation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SiteKey {
    path: String,
    fingerprint: String,
    primitive: Primitive,
}

#[derive(Debug, Clone)]
struct Site {
    key: SiteKey,
    line: usize,
    statement: String,
}

#[derive(Debug, Clone, Copy)]
struct AllowEntry {
    path: &'static str,
    fingerprint: &'static str,
    primitive: Primitive,
    class: OutputClass,
    rationale: &'static str,
    owning_test: &'static str,
}

impl AllowEntry {
    fn key(self) -> SiteKey {
        SiteKey {
            path: self.path.to_owned(),
            fingerprint: self.fingerprint.to_owned(),
            primitive: self.primitive,
        }
    }
}

#[path = "raw_output_policy/allowlist.rs"]
mod allowlist;
use allowlist::ALLOWLIST;

#[path = "raw_output_policy/owning_test.rs"]
mod owning_test;
use owning_test::validate as validate_owning_test;

#[path = "raw_output_policy/sink_analysis.rs"]
mod sink_analysis;
use sink_analysis::{render_receiver_is_glyph, write_method_has_document_argument};

#[derive(Debug, Default)]
struct PolicyDiff {
    unmatched: Vec<Site>,
    stale: Vec<AllowEntry>,
    duplicate_allowlist_keys: Vec<SiteKey>,
    invalid_metadata: Vec<(AllowEntry, String)>,
    violations: Vec<(Site, AllowEntry)>,
}

impl PolicyDiff {
    fn is_closed(&self) -> bool {
        self.unmatched.is_empty()
            && self.stale.is_empty()
            && self.duplicate_allowlist_keys.is_empty()
            && self.invalid_metadata.is_empty()
            && self.violations.is_empty()
    }

    fn render(&self) -> String {
        let mut report = String::new();
        if !self.unmatched.is_empty() {
            report.push_str("unmatched raw-output sites:\n");
            for site in &self.unmatched {
                report.push_str(&format_site(site));
            }
        }
        if !self.stale.is_empty() {
            report.push_str("stale raw-output allowlist entries:\n");
            for entry in &self.stale {
                report.push_str(&format!(
                    "  {} {} {} [{}]\n",
                    entry.path,
                    entry.primitive.as_str(),
                    entry.fingerprint,
                    entry.class.as_str()
                ));
            }
        }
        if !self.duplicate_allowlist_keys.is_empty() {
            report.push_str("duplicate raw-output allowlist keys:\n");
            for key in &self.duplicate_allowlist_keys {
                report.push_str(&format!(
                    "  {} {} {}\n",
                    key.path,
                    key.primitive.as_str(),
                    key.fingerprint
                ));
            }
        }
        if !self.invalid_metadata.is_empty() {
            report.push_str("allowlist entries with invalid rationale or owning test:\n");
            for (entry, reason) in &self.invalid_metadata {
                report.push_str(&format!(
                    "  {} {} {} -- {}\n",
                    entry.path,
                    entry.primitive.as_str(),
                    entry.fingerprint,
                    reason
                ));
            }
        }
        if !self.violations.is_empty() {
            report.push_str("current classified raw-output violations:\n");
            for (site, entry) in &self.violations {
                report.push_str(&format!(
                    "  {}:{} {} {} -- {} (owner: {})\n",
                    site.key.path,
                    site.line,
                    site.key.primitive.as_str(),
                    site.key.fingerprint,
                    entry.rationale,
                    entry.owning_test
                ));
            }
        }
        report
    }
}

fn compare_policy(sites: Vec<Site>, allowlist: &[AllowEntry]) -> PolicyDiff {
    let mut diff = PolicyDiff::default();
    let mut allowed = BTreeMap::new();
    let mut duplicate_keys = BTreeSet::new();

    for entry in allowlist {
        let key = entry.key();
        if allowed.insert(key.clone(), *entry).is_some() {
            duplicate_keys.insert(key);
        }
        if entry.rationale.trim().is_empty() {
            diff.invalid_metadata
                .push((*entry, "rationale is empty".to_owned()));
        }
        if let Err(reason) = validate_owning_test(entry.owning_test) {
            diff.invalid_metadata.push((*entry, reason));
        }
    }
    diff.duplicate_allowlist_keys = duplicate_keys.into_iter().collect();

    let mut discovered = BTreeMap::new();
    for site in sites {
        discovered.insert(site.key.clone(), site);
    }

    for (key, site) in &discovered {
        match allowed.get(key) {
            None => diff.unmatched.push(site.clone()),
            Some(entry) if entry.class == OutputClass::Violation => {
                diff.violations.push((site.clone(), *entry));
            }
            Some(_) => {}
        }
    }
    for (key, entry) in allowed {
        if !discovered.contains_key(&key) {
            diff.stale.push(entry);
        }
    }
    diff
}

fn format_site(site: &Site) -> String {
    format!(
        "  {}:{} {} {} => {}\n",
        site.key.path,
        site.line,
        site.key.primitive.as_str(),
        site.key.fingerprint,
        site.statement
    )
}

#[derive(Debug, Clone)]
struct Token {
    text: String,
    line: usize,
}

fn lex(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut line = 1;

    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                line += 1;
                index += 1;
            }
            byte if byte.is_ascii_whitespace() => index += 1,
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                let mut depth = 1usize;
                while index < bytes.len() && depth > 0 {
                    if bytes[index] == b'\n' {
                        line += 1;
                        index += 1;
                    } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
                        depth += 1;
                        index += 2;
                    } else if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                        depth -= 1;
                        index += 2;
                    } else {
                        index += 1;
                    }
                }
            }
            _ => {
                if let Some(end) = literal_end(source, index) {
                    let token_line = line;
                    let text = source[index..end].to_owned();
                    line += text.bytes().filter(|byte| *byte == b'\n').count();
                    tokens.push(Token {
                        text,
                        line: token_line,
                    });
                    index = end;
                    continue;
                }
                if is_ident_start(bytes[index]) {
                    let start = index;
                    index += 1;
                    while index < bytes.len() && is_ident_continue(bytes[index]) {
                        index += 1;
                    }
                    tokens.push(Token {
                        text: source[start..index].to_owned(),
                        line,
                    });
                } else {
                    let character = source[index..].chars().next().expect("valid UTF-8 source");
                    tokens.push(Token {
                        text: character.to_string(),
                        line,
                    });
                    index += character.len_utf8();
                }
            }
        }
    }
    tokens
}

fn literal_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let (prefix_len, quote) = if bytes.get(start) == Some(&b'"') {
        (0, b'"')
    } else if matches!(bytes.get(start), Some(b'b' | b'c')) && bytes.get(start + 1) == Some(&b'"') {
        (1, b'"')
    } else if bytes.get(start) == Some(&b'\'') {
        (0, b'\'')
    } else if bytes.get(start) == Some(&b'b') && bytes.get(start + 1) == Some(&b'\'') {
        (1, b'\'')
    } else {
        if let Some(end) = raw_string_end(source, start) {
            return Some(end);
        }
        return None;
    };

    let mut index = start + prefix_len + 1;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if quote == b'\'' && byte == b'\n' {
            return None;
        }
        if escaped {
            escaped = false;
            index += 1;
        } else if byte == b'\\' {
            escaped = true;
            index += 1;
        } else if byte == quote {
            return Some(index + 1);
        } else {
            index += 1;
        }
    }
    (quote == b'"').then_some(bytes.len())
}

fn raw_string_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let prefix_len = if bytes.get(start) == Some(&b'r') {
        1
    } else if matches!(bytes.get(start), Some(b'b' | b'c')) && bytes.get(start + 1) == Some(&b'r') {
        2
    } else {
        return None;
    };
    let mut index = start + prefix_len;
    let mut hashes = 0usize;
    while bytes.get(index) == Some(&b'#') {
        hashes += 1;
        index += 1;
    }
    if bytes.get(index) != Some(&b'"') {
        return None;
    }
    index += 1;
    while index < bytes.len() {
        if bytes[index] == b'"'
            && (0..hashes).all(|offset| bytes.get(index + 1 + offset) == Some(&b'#'))
        {
            return Some(index + 1 + hashes);
        }
        index += 1;
    }
    Some(bytes.len())
}

const fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

const fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tri {
    True,
    False,
    Unknown,
}

impl Tri {
    const fn not(self) -> Self {
        match self {
            Self::True => Self::False,
            Self::False => Self::True,
            Self::Unknown => Self::Unknown,
        }
    }

    const fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::False, _) | (_, Self::False) => Self::False,
            (Self::True, Self::True) => Self::True,
            _ => Self::Unknown,
        }
    }

    const fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::True, _) | (_, Self::True) => Self::True,
            (Self::False, Self::False) => Self::False,
            _ => Self::Unknown,
        }
    }
}

fn test_only_mask(tokens: &[Token]) -> Vec<bool> {
    // Be conservative: exclude an attributed item only when its cfg is
    // definitely false with `test = false`. Unknown features/platform cfgs
    // remain in the production scan.
    let mut excluded = vec![false; tokens.len()];
    let mut index = 0;
    while index < tokens.len() {
        if tokens[index].text != "#"
            || tokens.get(index + 1).map(|token| token.text.as_str()) != Some("[")
        {
            index += 1;
            continue;
        }

        let attribute_start = index;
        let mut cursor = index;
        let mut test_only = false;
        while cursor + 1 < tokens.len()
            && tokens[cursor].text == "#"
            && tokens[cursor + 1].text == "["
        {
            let Some(close) = matching_delimiter(tokens, cursor + 1, "[", "]") else {
                break;
            };
            test_only |= attribute_is_test_only(&tokens[cursor + 2..close]);
            cursor = close + 1;
        }
        if !test_only {
            index = cursor.max(index + 1);
            continue;
        }

        let item_end = item_end(tokens, cursor).unwrap_or(cursor);
        for value in &mut excluded[attribute_start..item_end.min(tokens.len())] {
            *value = true;
        }
        index = item_end.max(cursor + 1);
    }
    excluded
}

fn attribute_is_test_only(attribute: &[Token]) -> bool {
    if attribute.first().map(|token| token.text.as_str()) == Some("test") {
        return true;
    }
    if attribute.first().map(|token| token.text.as_str()) != Some("cfg")
        || attribute.get(1).map(|token| token.text.as_str()) != Some("(")
    {
        return false;
    }
    let mut cursor = 2;
    parse_cfg_expr(attribute, &mut cursor).is_some_and(|value| value == Tri::False)
}

fn parse_cfg_expr(tokens: &[Token], cursor: &mut usize) -> Option<Tri> {
    let name = tokens.get(*cursor)?.text.as_str();
    *cursor += 1;
    if matches!(name, "all" | "any" | "not")
        && tokens.get(*cursor).map(|token| token.text.as_str()) == Some("(")
    {
        *cursor += 1;
        let mut values = Vec::new();
        while *cursor < tokens.len()
            && tokens.get(*cursor).map(|token| token.text.as_str()) != Some(")")
        {
            values.push(parse_cfg_expr(tokens, cursor).unwrap_or(Tri::Unknown));
            if tokens.get(*cursor).map(|token| token.text.as_str()) == Some(",") {
                *cursor += 1;
            } else {
                break;
            }
        }
        if tokens.get(*cursor).map(|token| token.text.as_str()) == Some(")") {
            *cursor += 1;
        }
        return Some(match name {
            "all" => values.into_iter().fold(Tri::True, Tri::and),
            "any" => values.into_iter().fold(Tri::False, Tri::or),
            "not" => values.into_iter().next().unwrap_or(Tri::Unknown).not(),
            _ => unreachable!(),
        });
    }

    if tokens.get(*cursor).map(|token| token.text.as_str()) == Some("=") {
        *cursor += 1;
        *cursor += usize::from(*cursor < tokens.len());
        return Some(Tri::Unknown);
    }
    Some(if name == "test" {
        Tri::False
    } else {
        Tri::Unknown
    })
}

fn item_end(tokens: &[Token], start: usize) -> Option<usize> {
    let mut parens = 0usize;
    let mut brackets = 0usize;
    for index in start..tokens.len() {
        match tokens[index].text.as_str() {
            "(" => parens += 1,
            ")" => parens = parens.saturating_sub(1),
            "[" => brackets += 1,
            "]" => brackets = brackets.saturating_sub(1),
            ";" if parens == 0 && brackets == 0 => return Some(index + 1),
            "{" if parens == 0 && brackets == 0 => {
                return matching_delimiter(tokens, index, "{", "}").map(|end| end + 1);
            }
            _ => {}
        }
    }
    None
}

fn matching_delimiter(
    tokens: &[Token],
    open_index: usize,
    open: &str,
    close: &str,
) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open_index) {
        if token.text == open {
            depth += 1;
        } else if token.text == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

#[derive(Debug)]
struct FunctionSpan {
    name: String,
    parameters: Vec<String>,
    document_parameters: BTreeSet<String>,
    glyph_parameters: BTreeSet<String>,
    impl_type: Option<String>,
    open: usize,
    close: usize,
}

#[derive(Debug)]
struct ImplSpan {
    type_name: String,
    open: usize,
    close: usize,
}

fn impl_spans(tokens: &[Token], excluded: &[bool]) -> Vec<ImplSpan> {
    let mut spans = Vec::new();
    for index in 0..tokens.len() {
        if excluded[index] || tokens[index].text != "impl" {
            continue;
        }
        let mut cursor = index + 1;
        if tokens.get(cursor).map(|token| token.text.as_str()) == Some("<") {
            let Some(close) = matching_angle(tokens, cursor) else {
                continue;
            };
            cursor = close + 1;
        }
        let mut type_name = None;
        let mut after_for = false;
        let mut open = None;
        let mut angle_depth = 0usize;
        while cursor < tokens.len() {
            match tokens[cursor].text.as_str() {
                "<" => angle_depth += 1,
                ">" => angle_depth = angle_depth.saturating_sub(1),
                "for" if angle_depth == 0 => {
                    after_for = true;
                    type_name = None;
                }
                "{" if angle_depth == 0 => {
                    open = Some(cursor);
                    break;
                }
                ";" if angle_depth == 0 => break,
                text if angle_depth == 0
                    && is_path_ident(text)
                    && (after_for || type_name.is_none()) =>
                {
                    type_name = Some(text.to_owned());
                }
                _ => {}
            }
            cursor += 1;
        }
        let (Some(type_name), Some(open)) = (type_name, open) else {
            continue;
        };
        if let Some(close) = matching_delimiter(tokens, open, "{", "}") {
            spans.push(ImplSpan {
                type_name,
                open,
                close,
            });
        }
    }
    spans
}

fn matching_angle(tokens: &[Token], open_index: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open_index) {
        match token.text.as_str() {
            "<" => depth += 1,
            ">" => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn function_spans(tokens: &[Token], excluded: &[bool]) -> Vec<FunctionSpan> {
    let impls = impl_spans(tokens, excluded);
    let mut spans = Vec::new();
    for index in 0..tokens.len() {
        if excluded[index] || tokens[index].text != "fn" {
            continue;
        }
        let Some(name) = tokens.get(index + 1).map(|token| token.text.clone()) else {
            continue;
        };
        let mut cursor = index + 2;
        let mut parameters = Vec::new();
        let mut document_parameters = BTreeSet::new();
        let mut glyph_parameters = BTreeSet::new();
        let mut captured_parameters = false;
        let mut parens = 0usize;
        let mut brackets = 0usize;
        while cursor < tokens.len() {
            match tokens[cursor].text.as_str() {
                "(" => {
                    if !captured_parameters && parens == 0 && brackets == 0 {
                        if let Some(close) = matching_delimiter(tokens, cursor, "(", ")") {
                            (parameters, document_parameters, glyph_parameters) =
                                parameter_metadata(&tokens[cursor + 1..close]);
                            captured_parameters = true;
                        }
                    }
                    parens += 1;
                }
                ")" => parens = parens.saturating_sub(1),
                "[" => brackets += 1,
                "]" => brackets = brackets.saturating_sub(1),
                ";" if parens == 0 && brackets == 0 => break,
                "{" if parens == 0 && brackets == 0 => {
                    if let Some(close) = matching_delimiter(tokens, cursor, "{", "}") {
                        let impl_type = impls
                            .iter()
                            .filter(|span| span.open < cursor && close < span.close)
                            .min_by_key(|span| span.close - span.open)
                            .map(|span| span.type_name.clone());
                        spans.push(FunctionSpan {
                            name,
                            parameters,
                            document_parameters,
                            glyph_parameters,
                            impl_type,
                            open: cursor,
                            close,
                        });
                    }
                    break;
                }
                _ => {}
            }
            cursor += 1;
        }
    }
    spans
}

fn parameter_metadata(tokens: &[Token]) -> (Vec<String>, BTreeSet<String>, BTreeSet<String>) {
    let mut names = Vec::new();
    let mut documents = BTreeSet::new();
    let mut glyphs = BTreeSet::new();
    for (start, end) in split_top_level(tokens, ",") {
        let parameter = &tokens[start..end];
        let Some(colon) = parameter.iter().position(|token| token.text == ":") else {
            continue;
        };
        let Some(name) = parameter[..colon]
            .iter()
            .rev()
            .find(|token| is_path_ident(&token.text) && token.text != "mut")
            .map(|token| token.text.clone())
        else {
            continue;
        };
        if parameter[colon + 1..]
            .iter()
            .any(|token| token.text == "Document")
        {
            documents.insert(name.clone());
        }
        if parameter[colon + 1..]
            .iter()
            .any(|token| token.text == "Glyph")
        {
            glyphs.insert(name.clone());
        }
        names.push(name);
    }
    (names, documents, glyphs)
}

fn split_top_level(tokens: &[Token], delimiter: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    let mut angles = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        match token.text.as_str() {
            "(" => parens += 1,
            ")" => parens = parens.saturating_sub(1),
            "[" => brackets += 1,
            "]" => brackets = brackets.saturating_sub(1),
            "{" => braces += 1,
            "}" => braces = braces.saturating_sub(1),
            "<" => angles += 1,
            ">" => angles = angles.saturating_sub(1),
            text if text == delimiter
                && parens == 0
                && brackets == 0
                && braces == 0
                && angles == 0 =>
            {
                ranges.push((start, index));
                start = index + 1;
            }
            _ => {}
        }
    }
    if start < tokens.len() {
        ranges.push((start, tokens.len()));
    }
    ranges
}

#[derive(Debug, Default)]
struct OutputOrigins {
    bindings: Vec<BTreeSet<String>>,
    fields: BTreeSet<(String, String)>,
}

impl OutputOrigins {
    fn analyze(tokens: &[Token], functions: &[FunctionSpan]) -> Self {
        let mut origins = Self {
            bindings: vec![BTreeSet::new(); functions.len()],
            fields: BTreeSet::new(),
        };
        loop {
            let mut changed = false;
            for (function_index, function) in functions.iter().enumerate() {
                changed |= origins.discover_local_bindings(tokens, functions, function_index);
                changed |= origins.propagate_call_arguments(tokens, functions, function_index);
                changed |= origins.discover_fields(tokens, functions, function_index);
                debug_assert!(function.open < function.close);
            }
            if !changed {
                return origins;
            }
        }
    }

    fn discover_local_bindings(
        &mut self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        function_index: usize,
    ) -> bool {
        let function = &functions[function_index];
        let mut changed = false;
        let mut index = function.open + 1;
        while index < function.close {
            if tokens[index].text != "let" {
                index += 1;
                continue;
            }
            let Some(equal) = (index + 1..function.close)
                .find(|cursor| matches!(tokens[*cursor].text.as_str(), "=" | ";"))
            else {
                break;
            };
            if tokens[equal].text != "=" {
                index = equal + 1;
                continue;
            }
            let Some(end) = statement_end(tokens, equal + 1, function.close) else {
                break;
            };
            let binding = tokens[index + 1..equal]
                .iter()
                .find(|token| is_path_ident(&token.text) && token.text != "mut")
                .map(|token| token.text.clone());
            if let Some(binding) = binding {
                if self.expression_has_origin(tokens, functions, function_index, equal + 1, end) {
                    changed |= self.bindings[function_index].insert(binding);
                }
            }
            index += 1;
        }
        changed
    }

    fn propagate_call_arguments(
        &mut self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        caller_index: usize,
    ) -> bool {
        let caller = &functions[caller_index];
        let mut changes = Vec::new();
        for index in caller.open + 1..caller.close {
            if !is_path_ident(&tokens[index].text)
                || tokens.get(index + 1).map(|token| token.text.as_str()) != Some("(")
                || previous_is(tokens, index, "fn")
                || previous_is(tokens, index, ".")
            {
                continue;
            }
            let Some(close) = matching_delimiter(tokens, index + 1, "(", ")") else {
                continue;
            };
            if close > caller.close {
                continue;
            }
            let qualifier = if index >= 3
                && tokens[index - 1].text == ":"
                && tokens[index - 2].text == ":"
                && is_path_ident(&tokens[index - 3].text)
            {
                Some(tokens[index - 3].text.as_str())
            } else {
                None
            };
            let arguments = &tokens[index + 2..close];
            for (callee_index, callee) in functions.iter().enumerate() {
                if callee.name != tokens[index].text
                    || !callee_matches_qualifier(callee, qualifier, caller.impl_type.as_deref())
                {
                    continue;
                }
                for ((start, end), parameter) in split_top_level(arguments, ",")
                    .into_iter()
                    .zip(&callee.parameters)
                {
                    if self.expression_has_origin(
                        tokens,
                        functions,
                        caller_index,
                        index + 2 + start,
                        index + 2 + end,
                    ) {
                        changes.push((callee_index, parameter.clone()));
                    }
                }
            }
        }
        let mut changed = false;
        for (function_index, binding) in changes {
            changed |= self.bindings[function_index].insert(binding);
        }
        changed
    }

    fn discover_fields(
        &mut self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        function_index: usize,
    ) -> bool {
        let function = &functions[function_index];
        let Some(impl_type) = function.impl_type.as_deref() else {
            return false;
        };
        let mut changes = Vec::new();
        for index in function.open + 1..function.close {
            if tokens[index].text != "Self"
                || tokens.get(index + 1).map(|token| token.text.as_str()) != Some("{")
            {
                continue;
            }
            let Some(close) = matching_delimiter(tokens, index + 1, "{", "}") else {
                continue;
            };
            for (start, end) in split_top_level(&tokens[index + 2..close], ",") {
                let entry_start = index + 2 + start;
                let entry_end = index + 2 + end;
                let Some(field) = tokens[entry_start..entry_end]
                    .iter()
                    .find(|token| is_path_ident(&token.text))
                    .map(|token| token.text.clone())
                else {
                    continue;
                };
                let colon = (entry_start..entry_end).find(|cursor| tokens[*cursor].text == ":");
                let value_start = colon.map_or(entry_start, |colon| colon + 1);
                if self.expression_has_origin(
                    tokens,
                    functions,
                    function_index,
                    value_start,
                    entry_end,
                ) {
                    changes.push(field);
                }
            }
        }
        let mut changed = false;
        for field in changes {
            changed |= self.fields.insert((impl_type.to_owned(), field));
        }
        changed
    }

    fn expression_has_origin(
        &self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        function_index: usize,
        start: usize,
        end: usize,
    ) -> bool {
        let function = &functions[function_index];
        for index in start..end.min(tokens.len()) {
            if matches!(tokens[index].text.as_str(), "stdout" | "stderr")
                && tokens.get(index + 1).map(|token| token.text.as_str()) == Some("(")
                && is_io_path(tokens, index)
            {
                return true;
            }
            if matches!(
                tokens[index].text.as_str(),
                "stdout_writer" | "stderr_writer"
            ) && tokens.get(index + 1).map(|token| token.text.as_str()) == Some("(")
                && !previous_is(tokens, index, "fn")
            {
                return true;
            }
            if self.bindings[function_index].contains(&tokens[index].text) {
                return true;
            }
            if tokens[index].text == "self"
                && tokens.get(index + 1).map(|token| token.text.as_str()) == Some(".")
            {
                if let (Some(impl_type), Some(field)) = (
                    function.impl_type.as_deref(),
                    tokens.get(index + 2).map(|token| token.text.as_str()),
                ) {
                    if self
                        .fields
                        .contains(&(impl_type.to_owned(), field.to_owned()))
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn write_macro_has_origin(
        &self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        index: usize,
    ) -> bool {
        if tokens.get(index + 1).map(|token| token.text.as_str()) != Some("!")
            || tokens.get(index + 2).map(|token| token.text.as_str()) != Some("(")
        {
            return false;
        }
        let Some(close) = matching_delimiter(tokens, index + 2, "(", ")") else {
            return false;
        };
        let Some((start, end)) = split_top_level(&tokens[index + 3..close], ",")
            .into_iter()
            .next()
        else {
            return false;
        };
        let Some(function_index) = innermost_function_index(functions, index) else {
            return false;
        };
        self.expression_has_origin(
            tokens,
            functions,
            function_index,
            index + 3 + start,
            index + 3 + end,
        )
    }

    fn write_method_has_origin(
        &self,
        tokens: &[Token],
        functions: &[FunctionSpan],
        index: usize,
    ) -> bool {
        let Some(function_index) = innermost_function_index(functions, index) else {
            return false;
        };
        let start = expression_start(tokens, index.saturating_sub(1));
        self.expression_has_origin(tokens, functions, function_index, start, index - 1)
    }
}

fn callee_matches_qualifier(
    callee: &FunctionSpan,
    qualifier: Option<&str>,
    caller_impl: Option<&str>,
) -> bool {
    match qualifier {
        Some("Self") => callee.impl_type.as_deref() == caller_impl,
        Some(type_name) => callee.impl_type.as_deref() == Some(type_name),
        None => callee.impl_type.is_none(),
    }
}

fn statement_end(tokens: &[Token], start: usize, limit: usize) -> Option<usize> {
    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    for index in start..limit {
        match tokens[index].text.as_str() {
            "(" => parens += 1,
            ")" => parens = parens.saturating_sub(1),
            "[" => brackets += 1,
            "]" => brackets = brackets.saturating_sub(1),
            "{" => braces += 1,
            "}" if braces > 0 => braces -= 1,
            ";" if parens == 0 && brackets == 0 && braces == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

fn innermost_function_index(functions: &[FunctionSpan], index: usize) -> Option<usize> {
    functions
        .iter()
        .enumerate()
        .filter(|(_, function)| function.open < index && index < function.close)
        .min_by_key(|(_, function)| function.close - function.open)
        .map(|(index, _)| index)
}

fn expression_start(tokens: &[Token], end: usize) -> usize {
    let mut start = end;
    let mut parens = 0usize;
    let mut brackets = 0usize;
    while start > 0 {
        match tokens[start - 1].text.as_str() {
            ")" => parens += 1,
            "(" if parens > 0 => parens -= 1,
            "]" => brackets += 1,
            "[" if brackets > 0 => brackets -= 1,
            ";" | "," | "{" | "}" | "=" if parens == 0 && brackets == 0 => break,
            _ => {}
        }
        start -= 1;
    }
    start
}

fn primitive_at(
    path: &str,
    tokens: &[Token],
    index: usize,
    functions: &[FunctionSpan],
    origins: &OutputOrigins,
) -> Option<Primitive> {
    let text = tokens[index].text.as_str();
    if matches!(text, "print" | "println" | "eprint" | "eprintln" | "dbg")
        && tokens.get(index + 1).map(|token| token.text.as_str()) == Some("!")
    {
        return Some(Primitive::PrintMacro);
    }
    if matches!(text, "write" | "writeln")
        && origins.write_macro_has_origin(tokens, functions, index)
    {
        return Some(Primitive::DirectWrite);
    }
    if matches!(text, "write" | "write_all" | "write_fmt")
        && previous_is(tokens, index, ".")
        && tokens.get(index + 1).map(|token| token.text.as_str()) == Some("(")
        && !write_method_has_document_argument(tokens, functions, index)
        && origins.write_method_has_origin(tokens, functions, index)
    {
        return Some(Primitive::DirectWrite);
    }
    if matches!(text, "stdout" | "stderr")
        && tokens.get(index + 1).map(|token| token.text.as_str()) == Some("(")
        && is_io_path(tokens, index)
    {
        return Some(if text == "stdout" {
            Primitive::StdoutConstructor
        } else {
            Primitive::StderrConstructor
        });
    }
    if matches!(text, "stdout_writer" | "stderr_writer")
        && tokens.get(index + 1).map(|token| token.text.as_str()) == Some("(")
    {
        if previous_is(tokens, index, "fn") {
            return match path {
                "src/output.rs" => Some(Primitive::OutputRawHelper),
                "src/ui/writer.rs" => Some(Primitive::UiRawWriter),
                _ => None,
            };
        }
        if previous_is(tokens, index, ".") {
            return Some(Primitive::UiRawWriter);
        }
        if path_contains(tokens, index, "output") {
            return Some(Primitive::OutputRawHelper);
        }
    }
    if matches!(
        text,
        "write_stdout" | "write_stdout_line" | "write_stderr_line"
    ) && tokens.get(index + 1).map(|token| token.text.as_str()) == Some("(")
    {
        if (path == "src/output.rs" && previous_is(tokens, index, "fn"))
            || path_contains(tokens, index, "output")
        {
            return Some(Primitive::OutputRawHelper);
        }
    }
    if text == "with_writers"
        && ((path == "src/ui/writer.rs" && previous_is(tokens, index, "fn"))
            || (tokens.get(index + 1).map(|token| token.text.as_str()) == Some("(")
                && path_contains(tokens, index, "Ui")))
    {
        return Some(Primitive::UiWriterInjection);
    }
    if text == "render_plain" && tokens.get(index + 1).map(|token| token.text.as_str()) == Some("(")
    {
        if previous_is(tokens, index, ".")
            || (path == "src/ui/document.rs" && previous_is(tokens, index, "fn"))
        {
            return Some(Primitive::DocumentRender);
        }
    }
    if text == "render" && tokens.get(index + 1).map(|token| token.text.as_str()) == Some("(") {
        let document_call = previous_is(tokens, index, ".")
            && tokens.get(index + 2).map(|token| token.text.as_str()) == Some("&")
            && tokens.get(index + 3).map(|token| token.text.as_str()) != Some("mut")
            && !render_receiver_is_glyph(tokens, functions, index);
        let document_api = path == "src/ui/document.rs" && previous_is(tokens, index, "fn");
        if document_call || document_api {
            return Some(Primitive::DocumentRender);
        }
    }
    if text == "parse"
        && tokens.get(index + 1).map(|token| token.text.as_str()) == Some("(")
        && path_contains(tokens, index, "Cli")
    {
        return Some(Primitive::ClapParse);
    }
    None
}

fn is_io_path(tokens: &[Token], index: usize) -> bool {
    if index >= 3
        && tokens[index - 3].text == "io"
        && tokens[index - 2].text == ":"
        && tokens[index - 1].text == ":"
    {
        return true;
    }
    index >= 6
        && tokens[index - 6].text == "std"
        && tokens[index - 5].text == ":"
        && tokens[index - 4].text == ":"
        && tokens[index - 3].text == "io"
        && tokens[index - 2].text == ":"
        && tokens[index - 1].text == ":"
}

fn previous_is(tokens: &[Token], index: usize, expected: &str) -> bool {
    index > 0 && tokens[index - 1].text == expected
}

fn path_contains(tokens: &[Token], index: usize, expected: &str) -> bool {
    let start = index.saturating_sub(8);
    tokens[start..index]
        .iter()
        .rev()
        .take_while(|token| {
            matches!(token.text.as_str(), ":" | "." | "$") || is_path_ident(&token.text)
        })
        .any(|token| token.text == expected)
}

fn is_path_ident(text: &str) -> bool {
    text.bytes().next().is_some_and(is_ident_start) && text.bytes().all(is_ident_continue)
}

fn scan_source(path: &str, source: &str) -> Vec<Site> {
    let tokens = lex(source);
    let excluded = test_only_mask(&tokens);
    let functions = function_spans(&tokens, &excluded);
    let origins = OutputOrigins::analyze(&tokens, &functions);
    let mut ordinals: BTreeMap<(String, Primitive), usize> = BTreeMap::new();
    let mut sites = Vec::new();

    for index in 0..tokens.len() {
        if excluded[index] {
            continue;
        }
        let Some(primitive) = primitive_at(path, &tokens, index, &functions, &origins) else {
            continue;
        };
        let owner = functions
            .iter()
            .filter(|function| function.open < index && index < function.close)
            .min_by_key(|function| function.close - function.open)
            .map(|function| function.name.as_str())
            .unwrap_or("<module>");
        let statement = normalized_statement(&tokens, index);
        let ordinal = ordinals.entry((owner.to_owned(), primitive)).or_default();
        *ordinal += 1;
        let fingerprint = format!(
            "{owner}#{}@{:016x}",
            *ordinal,
            fnv1a64(statement.as_bytes())
        );
        sites.push(Site {
            key: SiteKey {
                path: path.to_owned(),
                fingerprint,
                primitive,
            },
            line: tokens[index].line,
            statement,
        });
    }
    sites
}

fn normalized_statement(tokens: &[Token], index: usize) -> String {
    let mut start = index;
    let mut reverse_depth = 0usize;
    while start > 0 {
        let token = tokens[start - 1].text.as_str();
        match token {
            ")" | "]" => reverse_depth += 1,
            "(" | "[" if reverse_depth > 0 => reverse_depth -= 1,
            ";" | "{" | "}" | "," if reverse_depth == 0 => break,
            _ => {}
        }
        start -= 1;
    }

    let mut end = index;
    let mut depth = 0usize;
    while end < tokens.len() {
        let token = tokens[end].text.as_str();
        match token {
            "(" | "[" => depth += 1,
            ")" | "]" => depth = depth.saturating_sub(1),
            ";" | "," if depth == 0 => {
                end += 1;
                break;
            }
            "{" | "}" if depth == 0 && end > index => break,
            _ => {}
        }
        end += 1;
    }
    tokens[start..end.min(tokens.len())]
        .iter()
        .map(|token| token.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

const fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        index += 1;
    }
    hash
}

fn package_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if manifest.join("src").is_dir() {
        return manifest;
    }
    if let (Ok(source_dir), Ok(workspace)) = (env::var("TEST_SRCDIR"), env::var("TEST_WORKSPACE")) {
        let runfiles = PathBuf::from(source_dir)
            .join(workspace)
            .join("crates/ctx-cli");
        if runfiles.join("src").is_dir() {
            return runfiles;
        }
    }
    panic!(
        "cannot resolve crates/ctx-cli source root from CARGO_MANIFEST_DIR={}",
        env!("CARGO_MANIFEST_DIR")
    );
}

fn production_source_paths(root: &Path) -> Vec<PathBuf> {
    fn visit(directory: &Path, paths: &mut Vec<PathBuf>) {
        let mut entries = fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|error| panic!("read {} entry: {error}", directory.display()));
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().and_then(|name| name.to_str()) != Some("tests") {
                    visit(&path, paths);
                }
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs")
                && !is_test_source_file(&path)
            {
                paths.push(path);
            }
        }
    }

    let mut paths = vec![root.join("build.rs")];
    visit(&root.join("src"), &mut paths);
    paths.sort();
    paths
}

fn is_test_source_file(path: &Path) -> bool {
    // Keep this aligned with RUST_PROD_SRC_EXCLUDES. Do not add product files
    // here to silence a finding; narrow a detector and add a scanner test
    // instead.
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || name.starts_with("test_support")
        || path
            .components()
            .any(|component| component.as_os_str() == "tests")
}

fn scan_package() -> Vec<Site> {
    let root = package_root();
    let mut sites = Vec::new();
    for path in production_source_paths(&root) {
        let relative = path
            .strip_prefix(&root)
            .expect("package source belongs to package root")
            .to_string_lossy()
            .replace('\\', "/");
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        sites.extend(scan_source(&relative, &source));
    }
    sites
}

#[test]
fn production_raw_output_inventory_is_closed() {
    let diff = compare_policy(scan_package(), ALLOWLIST);
    assert!(diff.is_closed(), "{}", diff.render());
}

#[path = "raw_output_policy/self_tests.rs"]
mod self_tests;
