use url::Url;

fn strip_ansi_sequences(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut idx = 0usize;
    while idx < chars.len() {
        let ch = chars[idx];
        if ch == '\u{1b}' {
            idx += 1;
            if idx < chars.len() {
                if chars[idx] == '[' {
                    idx += 1;
                    while idx < chars.len() {
                        let c = chars[idx];
                        idx += 1;
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                    continue;
                }
                if chars[idx] == ']' {
                    idx += 1;
                    let mut payload = String::new();
                    while idx < chars.len() {
                        let c = chars[idx];
                        if c == '\u{7}' {
                            idx += 1;
                            break;
                        }
                        if c == '\u{1b}' && (idx + 1) < chars.len() && chars[idx + 1] == '\\' {
                            idx += 2;
                            break;
                        }
                        payload.push(c);
                        idx += 1;
                    }
                    if let Some(url) = payload
                        .strip_prefix("8;;")
                        .filter(|value| !value.is_empty())
                    {
                        out.push(' ');
                        out.push_str(url);
                        out.push(' ');
                    }
                    continue;
                }
            }
            continue;
        }
        if ch != '\r' && ch != '\u{7}' {
            out.push(ch);
        }
        idx += 1;
    }
    out
}

fn is_auth_url_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '-' | '.'
                | '_'
                | '~'
                | ':'
                | '/'
                | '?'
                | '#'
                | '['
                | ']'
                | '@'
                | '!'
                | '$'
                | '&'
                | '\''
                | '('
                | ')'
                | '*'
                | '+'
                | ','
                | ';'
                | '='
                | '%'
        )
}

fn matches_auth_url_scheme_from(chars: &[char], start: usize, scheme: &str) -> bool {
    let mut idx = start;
    for expected in scheme.chars() {
        while idx < chars.len() && chars[idx].is_whitespace() {
            idx += 1;
        }
        if idx >= chars.len() || chars[idx].to_ascii_lowercase() != expected {
            return false;
        }
        idx += 1;
    }
    true
}

fn find_auth_url_start(chars: &[char], from_idx: usize) -> Option<usize> {
    let mut idx = from_idx;
    while idx < chars.len() {
        if chars[idx].eq_ignore_ascii_case(&'h')
            && (matches_auth_url_scheme_from(chars, idx, "https://")
                || matches_auth_url_scheme_from(chars, idx, "http://"))
        {
            return Some(idx);
        }
        idx += 1;
    }
    None
}

pub(super) fn extract_auth_url(text: &str) -> Option<String> {
    let normalized = strip_ansi_sequences(text);
    let chars: Vec<char> = normalized.chars().collect();
    let mut idx = 0usize;
    while idx < chars.len() {
        idx = find_auth_url_start(&chars, idx)?;
        let mut end = idx;
        let mut candidate = String::new();
        while end < chars.len() {
            let ch = chars[end];
            if is_auth_url_char(ch) {
                candidate.push(ch);
                end += 1;
                continue;
            }
            if ch.is_whitespace() {
                let mut probe = end;
                while probe < chars.len() && chars[probe].is_whitespace() {
                    probe += 1;
                }
                if probe < chars.len() && is_auth_url_char(chars[probe]) {
                    let next_fragment_start = probe;
                    while probe < chars.len() && is_auth_url_char(chars[probe]) {
                        probe += 1;
                    }
                    let next_fragment: String = chars[next_fragment_start..probe].iter().collect();
                    if should_continue_auth_url_after_break(&candidate, &next_fragment) {
                        end = next_fragment_start;
                        continue;
                    }
                }
            }
            break;
        }
        let trimmed = candidate
            .trim_matches(|c: char| {
                matches!(
                    c,
                    '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | '.'
                )
            })
            .to_string();
        if let Ok(parsed) = Url::parse(&trimmed) {
            if matches!(parsed.scheme(), "http" | "https") {
                return Some(trimmed);
            }
        }
        idx = end.saturating_add(1);
    }
    None
}

fn should_continue_auth_url_after_break(current: &str, next_fragment: &str) -> bool {
    if current.is_empty() || next_fragment.is_empty() {
        return false;
    }
    if fragment_starts_full_auth_url(next_fragment) && Url::parse(current).is_ok() {
        return false;
    }
    if auth_scheme_can_continue_across_break(current, next_fragment) {
        return true;
    }
    if next_fragment.chars().any(|ch| !ch.is_ascii_alphanumeric()) {
        return true;
    }
    if next_fragment
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_digit())
    {
        return true;
    }
    matches!(
        current.chars().last(),
        Some('%' | ':' | '=' | '&' | '?' | '/' | '#' | '-' | '_')
    )
}

fn auth_scheme_can_continue_across_break(current: &str, next_fragment: &str) -> bool {
    let compact_current: String = current
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    let compact_next: String = next_fragment
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    if compact_current.is_empty() || compact_next.is_empty() {
        return false;
    }
    let combined = format!("{compact_current}{compact_next}");
    ["https://", "http://"]
        .into_iter()
        .any(|scheme| scheme.starts_with(&combined))
}

fn fragment_starts_full_auth_url(value: &str) -> bool {
    let compact = value
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    compact.starts_with("https://") || compact.starts_with("http://")
}
