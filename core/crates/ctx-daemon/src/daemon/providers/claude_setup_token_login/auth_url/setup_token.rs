fn is_claude_setup_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'
}

fn is_setup_token_fragment(value: &str) -> bool {
    !value.is_empty() && value.chars().all(is_claude_setup_token_char)
}

fn leading_setup_token_fragment(value: &str) -> &str {
    let mut end = 0usize;
    for (idx, ch) in value.char_indices() {
        if is_claude_setup_token_char(ch) {
            end = idx + ch.len_utf8();
            continue;
        }
        break;
    }
    &value[..end]
}

fn is_setup_token_continuation_fragment(value: &str) -> bool {
    if !is_setup_token_fragment(value) {
        return false;
    }
    value
        .chars()
        .any(|ch| ch.is_ascii_digit() || ch == '-' || ch == '_')
}

fn trim_known_setup_token_prose_suffix(token: &str) -> String {
    const PROSE_CANONICAL: &str = "StorethistokensecurelyYouwontbeabletoseeitagain";
    const MIN_MATCH_LEN: usize = 5;

    let mut out = token.to_string();
    for phrase in [
        PROSE_CANONICAL,
        "storethistokensecurelyyouwontbeabletoseeitagain",
    ] {
        let max = std::cmp::min(out.len(), phrase.len());
        let mut truncate_at: Option<usize> = None;
        for len in (MIN_MATCH_LEN..=max).rev() {
            if out.ends_with(&phrase[..len]) {
                truncate_at = Some(out.len() - len);
                break;
            }
        }
        if let Some(idx) = truncate_at {
            out.truncate(idx);
        }
    }
    out
}

pub(in crate::daemon::providers::claude_setup_token_login) fn extract_claude_setup_token(
    output: &str,
) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let Some(start) = line.find("sk-ant-oat") else {
            continue;
        };
        let first_fragment = leading_setup_token_fragment(&line[start..]);
        if first_fragment.is_empty() {
            continue;
        }
        let mut token = first_fragment.to_string();
        for next in lines.iter().skip(idx + 1) {
            let trimmed = next.trim();
            if trimmed.is_empty() {
                break;
            }
            let fragment = leading_setup_token_fragment(trimmed);
            if !is_setup_token_continuation_fragment(fragment) {
                break;
            }
            token.push_str(fragment);
        }
        let cleaned = trim_known_setup_token_prose_suffix(&token);
        if cleaned.len() > 40 {
            return Some(cleaned);
        }
    }
    None
}
