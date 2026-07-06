#[allow(unused_imports)]
use super::*;

pub(crate) fn pad_table_cell(value: &str, width: usize) -> String {
    let len = value.chars().count();
    if len >= width {
        value.to_owned()
    } else {
        format!("{value}{}", " ".repeat(width - len))
    }
}
