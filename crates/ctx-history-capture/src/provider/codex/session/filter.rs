pub(crate) fn should_parse_codex_session_line(line: &[u8]) -> bool {
    if contains_bytes(line, br#""type":"session_meta""#)
        || contains_bytes(line, br#""type":"compacted""#)
    {
        return true;
    }

    if contains_bytes(line, br#""type":"event_msg""#) {
        return codex_session_event_msg_may_touch_file(line);
    }

    if !contains_bytes(line, br#""type":"response_item""#) {
        return false;
    }

    if contains_bytes(line, br#""type":"message""#)
        && (contains_bytes(line, br#""role":"user""#)
            || contains_bytes(line, br#""role":"assistant""#)
            || contains_bytes(line, br#""role":"system""#)
            || contains_bytes(line, br#""role":"developer""#))
    {
        return true;
    }

    if codex_session_line_may_touch_file(line) {
        return true;
    }

    contains_bytes(line, br#""type":"function_call""#)
        || contains_bytes(line, br#""type":"custom_tool_call""#)
        || contains_bytes(line, br#""type":"web_search_call""#)
        || contains_bytes(line, br#""type":"tool_search_call""#)
        || contains_bytes(line, br#""type":"function_call_output""#)
        || contains_bytes(line, br#""type":"custom_tool_call_output""#)
        || contains_bytes(line, br#""type":"tool_search_output""#)
        || contains_bytes(line, br#""type":"reasoning""#)
}
pub(crate) fn codex_session_event_msg_may_touch_file(line: &[u8]) -> bool {
    contains_bytes(line, br#""patch_apply_end""#)
        || contains_bytes(line, b"apply_patch")
        || contains_bytes(line, b"*** Begin Patch")
        || contains_bytes(line, b"changes")
}
pub(crate) fn codex_session_line_may_touch_file(line: &[u8]) -> bool {
    contains_bytes(line, br#""type":"response_item""#)
        && (contains_bytes(line, b"apply_patch")
            || contains_bytes(line, b"*** Begin Patch")
            || contains_bytes(line, b"write_file")
            || contains_bytes(line, b"edit_file")
            || contains_bytes(line, b"str_replace")
            || contains_bytes(line, b"file_path")
            || contains_bytes(line, b"TargetFile"))
}
pub(crate) fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    find_bytes(haystack, needle).is_some()
}
pub(crate) fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
