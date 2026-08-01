pub(crate) mod nativepath;

#[cfg(test)]
mod tests {
    #[test]
    fn retained_rows_have_no_resolver_era_content_reference() {
        let sources = [
            include_str!("claude/nativepath/rows.rs"),
            include_str!("claude/nativepath/record.rs"),
            include_str!("claude/nativepath/record/value_decoding.rs"),
        ]
        .join("\n");
        for removed in [concat!("Content", "Ref"), concat!("complete_body", "_ref")] {
            assert!(!sources.contains(removed), "found {removed}");
        }
    }
}
