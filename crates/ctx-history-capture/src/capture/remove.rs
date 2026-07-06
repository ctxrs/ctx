#[allow(unused_imports)]
use super::*;

pub(crate) fn remove_xml_like_block(input: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut output = input.to_owned();
    while let Some(start) = output.find(&open) {
        let Some(relative_end) = output[start + open.len()..].find(&close) else {
            output.replace_range(start..start + open.len(), "");
            continue;
        };
        let end = start + open.len() + relative_end + close.len();
        output.replace_range(start..end, "");
    }
    output
}
