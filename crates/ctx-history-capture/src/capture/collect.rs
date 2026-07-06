#[allow(unused_imports)]
use super::*;

pub(crate) fn collect_patch_file_touches(value: &Value, out: &mut Vec<FileTouchDraft>) {
    match value {
        Value::String(text) => {
            if text.contains("*** Begin Patch") {
                out.extend(parse_apply_patch_file_touches(text));
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_patch_file_touches(item, out);
            }
        }
        Value::Object(object) => {
            for value in object.values() {
                collect_patch_file_touches(value, out);
            }
        }
        _ => {}
    }
}

pub(crate) fn collect_structured_file_touches(value: &Value, out: &mut Vec<FileTouchDraft>) {
    collect_structured_file_touches_with_context(value, out, None);
}

pub(crate) fn collect_structured_file_touches_with_context(
    value: &Value,
    out: &mut Vec<FileTouchDraft>,
    inherited_kind: Option<FileChangeKind>,
) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_structured_file_touches_with_context(item, out, inherited_kind);
            }
        }
        Value::Object(object) => {
            let operation_kind = object_operation_hint_kind(object);
            let object_kind = operation_kind.or(inherited_kind);
            collect_structured_file_touch_object(object, out, object_kind);
            for value in object.values() {
                collect_structured_file_touches_with_context(value, out, object_kind);
            }
        }
        _ => {}
    }
}
