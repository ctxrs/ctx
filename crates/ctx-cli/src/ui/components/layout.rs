use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
    let Some(width) = width else {
        return split_owned_lines(text);
    };
    let width = width.max(1);
    let mut wrapped = Vec::new();

    for logical_line in text.split('\n') {
        let logical_line = crate::ui::document::neutralize_controls(logical_line);
        wrap_logical_line(&logical_line, width, &mut wrapped);
    }

    if wrapped.is_empty() {
        wrapped.push(String::new());
    }
    wrapped
}

fn split_owned_lines(text: &str) -> Vec<String> {
    let lines = text.split('\n').map(ToOwned::to_owned).collect::<Vec<_>>();
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
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
    for character in word.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        let current_width = display_width(current);
        if !current.is_empty() && current_width.saturating_add(character_width) > width {
            output.push(std::mem::take(current));
        }
        current.push(character);
    }
}
