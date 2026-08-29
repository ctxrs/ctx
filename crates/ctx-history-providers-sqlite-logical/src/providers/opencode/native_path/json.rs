use std::{collections::BTreeSet, fmt};

use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

use super::super::{normalization::opencode_event_time, schema::OpenCodeSqliteDialect};
use super::model::{OpenCodeNativeRejectionKind, OpenCodeNativeSchemaFamily};

mod audit;
mod output;

use audit::audit_json;
use output::*;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct OpenCodeRetainedJson {
    pub(super) effective_type: String,
    pub(super) role: String,
    pub(super) body: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct OpenCodeOutputJson {
    pub(super) diagnostic: Option<OpenCodeRetainedJson>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum OpenCodeJsonProjection {
    Retained(OpenCodeRetainedJson),
    Output(OpenCodeOutputJson),
    Rejected(OpenCodeNativeRejectionKind),
    RejectedWithReason(OpenCodeNativeRejectionKind, String),
}

pub(super) fn project_json(
    data: &str,
    column_type: &str,
    parent_data: Option<&str>,
    family: OpenCodeNativeSchemaFamily,
    dialect: &OpenCodeSqliteDialect,
    has_explicit_event_time: &mut bool,
) -> OpenCodeJsonProjection {
    *has_explicit_event_time = false;
    let direct_column_output = is_direct_output_token(column_type);
    let body = match audit_json(data) {
        Ok(value) => value,
        Err(()) => {
            return OpenCodeJsonProjection::Rejected(if direct_column_output {
                OpenCodeNativeRejectionKind::MalformedResultJson
            } else {
                OpenCodeNativeRejectionKind::MalformedJson
            });
        }
    };
    *has_explicit_event_time = family != OpenCodeNativeSchemaFamily::MessagePart
        && body.value.pointer("/time/created").is_some();
    let parent = match parent_data.map(audit_json).transpose() {
        Ok(value) => value,
        Err(()) => {
            return OpenCodeJsonProjection::Rejected(if direct_column_output {
                OpenCodeNativeRejectionKind::MalformedResultJson
            } else {
                OpenCodeNativeRejectionKind::MalformedJson
            });
        }
    };
    if family != OpenCodeNativeSchemaFamily::MessagePart {
        if let Err(error) = opencode_event_time(&body.value, dialect) {
            return OpenCodeJsonProjection::RejectedWithReason(
                OpenCodeNativeRejectionKind::InvalidTimestamp,
                error.to_string(),
            );
        }
    }
    let body_type = object_text(&body.value, "type");
    let body_role = object_text(&body.value, "role");
    let parent_role = parent
        .as_ref()
        .and_then(|value| object_text(&value.value, "role"));
    let effective_type = effective_type(column_type, body_role, body_type, parent_role);
    if is_ignored_type(family, &effective_type) {
        if body.duplicate_key || parent.as_ref().is_some_and(|value| value.duplicate_key) {
            return OpenCodeJsonProjection::Rejected(OpenCodeNativeRejectionKind::MalformedJson);
        }
        // OpenCode file carriers may contain inline data URLs. Recognize valid
        // file parts before generic output detection without copying attachment
        // bytes or metadata into Core.
        return OpenCodeJsonProjection::Output(OpenCodeOutputJson { diagnostic: None });
    }
    let output = direct_column_output
        || is_direct_output_token(&effective_type)
        || body.forbidden_output
        || parent.as_ref().is_some_and(|value| value.forbidden_output);
    if output {
        if body.duplicate_key || parent.as_ref().is_some_and(|value| value.duplicate_key) {
            return OpenCodeJsonProjection::Output(OpenCodeOutputJson { diagnostic: None });
        }
        return project_output(&body.value, &effective_type);
    }
    if body.duplicate_key || parent.as_ref().is_some_and(|value| value.duplicate_key) {
        return OpenCodeJsonProjection::Rejected(OpenCodeNativeRejectionKind::MalformedJson);
    }
    if is_tool_token(&effective_type) {
        if tool_call_is_retained(&body.value) {
            // Continue below and retain the input-side projection.
        } else {
            return project_output(&body.value, &effective_type);
        }
    } else if !is_retained_type(Some(family), &effective_type) {
        return OpenCodeJsonProjection::Rejected(OpenCodeNativeRejectionKind::UnknownRecordType);
    }
    let role = if family == OpenCodeNativeSchemaFamily::MessagePart {
        first_nonempty(&[parent_role, body_role])
    } else {
        first_nonempty(&[body_role, Some(effective_type.as_str()), parent_role])
    }
    .unwrap_or("assistant")
    .to_owned();
    OpenCodeJsonProjection::Retained(OpenCodeRetainedJson {
        effective_type,
        role,
        body: body.value,
    })
}

pub(super) fn malformed_json_projection(column_type: &str) -> OpenCodeJsonProjection {
    OpenCodeJsonProjection::Rejected(if is_direct_output_token(column_type) {
        OpenCodeNativeRejectionKind::MalformedResultJson
    } else {
        OpenCodeNativeRejectionKind::MalformedJson
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;

    fn project(
        data: &str,
        column_type: &str,
        parent: Option<&str>,
        family: OpenCodeNativeSchemaFamily,
    ) -> (OpenCodeJsonProjection, bool) {
        let mut explicit_time = false;
        let projection = project_json(
            data,
            column_type,
            parent,
            family,
            &OPENCODE_SQLITE_DIALECT,
            &mut explicit_time,
        );
        (projection, explicit_time)
    }

    #[test]
    fn direct_projection_retains_body_role_and_explicit_timestamp_once() {
        let (projection, explicit_time) = project(
            r#"{"role":"user","time":{"created":1},"text":"hello"}"#,
            "message",
            None,
            OpenCodeNativeSchemaFamily::SessionMessageSeq,
        );
        assert!(explicit_time);
        let OpenCodeJsonProjection::Retained(retained) = projection else {
            panic!("ordinary message was not retained");
        };
        assert_eq!(retained.effective_type, "user");
        assert_eq!(retained.role, "user");
        assert_eq!(retained.body["text"], "hello");
    }

    #[test]
    fn direct_projection_retains_all_exact_output_bodies() {
        let (success, _) = project(
            r#"{"type":"tool_result","success":true,"output":"ok"}"#,
            "result",
            None,
            OpenCodeNativeSchemaFamily::SessionMessageSeq,
        );
        let OpenCodeJsonProjection::Output(OpenCodeOutputJson {
            diagnostic: Some(success),
        }) = success
        else {
            panic!("output was not retained");
        };
        assert_eq!(success.body["success"], true);
        assert_eq!(success.body["output"], "ok");

        let (failure, _) = project(
            r#"{"type":"tool_result","exit_code":7,"command":"false"}"#,
            "result",
            None,
            OpenCodeNativeSchemaFamily::SessionMessageSeq,
        );
        let OpenCodeJsonProjection::Output(OpenCodeOutputJson {
            diagnostic: Some(diagnostic),
        }) = failure
        else {
            panic!("failed output did not retain a diagnostic");
        };
        assert_eq!(diagnostic.body["exit_code"], 7);
        assert_eq!(diagnostic.body["command"], "false");

        let (empty_role, _) = project(
            r#"{"type":"tool_result","role":"","output":"ok"}"#,
            "result",
            None,
            OpenCodeNativeSchemaFamily::SessionMessageSeq,
        );
        let OpenCodeJsonProjection::Output(OpenCodeOutputJson {
            diagnostic: Some(diagnostic),
        }) = empty_role
        else {
            panic!("empty output role did not retain a diagnostic");
        };
        assert_eq!(diagnostic.role, "tool");
        assert_eq!(diagnostic.body["role"], "");
    }

    #[test]
    fn direct_projection_preserves_malformed_and_duplicate_fail_closed_classes() {
        assert!(matches!(
            project(
                "{",
                "message",
                None,
                OpenCodeNativeSchemaFamily::SessionMessageSeq,
            )
            .0,
            OpenCodeJsonProjection::Rejected(OpenCodeNativeRejectionKind::MalformedJson)
        ));
        assert!(matches!(
            project(
                "{",
                "result",
                None,
                OpenCodeNativeSchemaFamily::SessionMessageSeq,
            )
            .0,
            OpenCodeJsonProjection::Rejected(OpenCodeNativeRejectionKind::MalformedResultJson)
        ));
        assert!(matches!(
            project(
                r#"{"output":"first","output":"second"}"#,
                "result",
                None,
                OpenCodeNativeSchemaFamily::SessionMessageSeq,
            )
            .0,
            OpenCodeJsonProjection::Output(OpenCodeOutputJson { diagnostic: None })
        ));
    }

    #[test]
    fn direct_projection_preserves_parent_role_and_invalid_timestamp_rejection() {
        let (part, explicit_time) = project(
            r#"{"type":"text","text":"answer"}"#,
            "message",
            Some(r#"{"role":"assistant"}"#),
            OpenCodeNativeSchemaFamily::MessagePart,
        );
        assert!(!explicit_time);
        let OpenCodeJsonProjection::Retained(retained) = part else {
            panic!("message part was not retained");
        };
        assert_eq!(retained.role, "assistant");

        assert!(matches!(
            project(
                r#"{"role":"user","time":{"created":"bad"}}"#,
                "message",
                None,
                OpenCodeNativeSchemaFamily::SessionMessageSeq,
            )
            .0,
            OpenCodeJsonProjection::RejectedWithReason(
                OpenCodeNativeRejectionKind::InvalidTimestamp,
                _
            )
        ));
    }

    #[test]
    fn current_file_parts_are_known_ignored_carriers() {
        let (projection, explicit_time) = project(
            r#"{"type":"file","mime":"image/png","filename":"diagram.png","url":"data:image/png;base64,must-not-be-indexed"}"#,
            "part",
            Some(r#"{"role":"user"}"#),
            OpenCodeNativeSchemaFamily::MessagePart,
        );

        assert!(!explicit_time);
        assert_eq!(
            projection,
            OpenCodeJsonProjection::Output(OpenCodeOutputJson { diagnostic: None })
        );
    }

    #[test]
    fn current_file_parts_cannot_enter_generic_output_retention() {
        let (projection, _) = project(
            r#"{"type":"file","mime":"image/png","url":"data:image/png;base64,must-not-be-indexed","metadata":{"output":"must-not-be-indexed","result":"must-not-be-indexed"}}"#,
            "part",
            Some(r#"{"role":"user"}"#),
            OpenCodeNativeSchemaFamily::MessagePart,
        );

        assert_eq!(
            projection,
            OpenCodeJsonProjection::Output(OpenCodeOutputJson { diagnostic: None })
        );
    }
}
