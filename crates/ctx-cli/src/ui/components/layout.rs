use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr;

pub(super) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(crate::ui::document::neutralize_controls(text).as_str())
}

pub(super) fn pad(width: usize) -> String {
    " ".repeat(width)
}

pub(super) fn pad_after(text: &str, target_width: usize) -> String {
    pad(target_width.saturating_sub(display_width(text)))
}

pub(super) fn wrap_text(text: &str, width: Option<usize>) -> Vec<String> {
    let text = crate::ui::document::neutralize_controls(text);
    let Some(width) = width else {
        return vec![text];
    };
    let width = width.max(1);
    let mut wrapped = Vec::new();

    wrap_logical_line(&text, width, &mut wrapped);

    if wrapped.is_empty() {
        wrapped.push(String::new());
    }
    wrapped
}

fn wrap_logical_line(line: &str, width: usize, output: &mut Vec<String>) {
    let mut current = String::new();
    for word in line.split_whitespace() {
        if current.is_empty() {
            push_word(word, width, &mut current, output);
            continue;
        }

        let joined_width = display_width(&current)
            .saturating_add(1)
            .saturating_add(display_width(word));
        if joined_width <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            output.push(std::mem::take(&mut current));
            push_word(word, width, &mut current, output);
        }
    }

    if !current.is_empty() || line.trim().is_empty() {
        output.push(current);
    }
}

fn push_word(word: &str, width: usize, current: &mut String, output: &mut Vec<String>) {
    if is_copyable_atom(word) {
        current.push_str(word);
        return;
    }
    for grapheme in word.graphemes(true) {
        let grapheme_width = display_width(grapheme);
        let current_width = display_width(current);
        if !current.is_empty() && current_width.saturating_add(grapheme_width) > width {
            output.push(std::mem::take(current));
        }
        current.push_str(grapheme);
    }
}

pub(crate) fn is_copyable_atom(word: &str) -> bool {
    word.contains("://") || word.starts_with('-') || word.matches('-').count() >= 4
}

#[cfg(test)]
mod tests {
    use super::wrap_text;

    #[test]
    fn wrapping_preserves_graphemes_and_copyable_identifiers() {
        let family = "👨‍👩‍👧‍👦👨‍👩‍👧‍👦";
        assert_eq!(wrap_text(family, Some(2)), vec!["👨‍👩‍👧‍👦", "👨‍👩‍👧‍👦"]);

        let id = "00000000-0000-0000-0000-000000000001";
        assert_eq!(wrap_text(id, Some(8)), vec![id]);
    }
}
