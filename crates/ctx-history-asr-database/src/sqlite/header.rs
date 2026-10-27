use crate::{records::session_ref, Provider};
use serde_json::{json, Value};

pub fn header(provider: Provider, id: &str, _row: &Value) -> Value {
    let mut losses = vec![
        "revision_history",
        "order",
        "message.author",
        "copies",
        "usage",
        "branches",
        "display_content",
        "exec.exit_code",
        "_rs.ctx/native_row_limit",
        "_rs.ctx/vendor_only",
    ];
    if provider != Provider::Goose {
        losses.push("inference");
    }
    if provider == Provider::Lingma {
        losses.extend([
            "tool_result.content",
            "reasoning",
            "usage",
            "cost",
            "_com.aliyun.lingma/answer_summary",
        ]);
    }
    if provider == Provider::Warp {
        losses.extend([
            "timestamps",
            "usage",
            "cost",
            "tool_result.content",
            "_dev.warp.agent/unsupported_protobuf_semantics",
        ]);
    }
    if matches!(provider, Provider::Crush | Provider::Goose) {
        losses.push("provider.identity");
    }
    json!({"type":"session","formatVersion":"0.1","agent":{"name":provider.agent()},"sessionId":id,
        "producer":{"name":"ctx","version":env!("CARGO_PKG_VERSION"),"method":if provider==Provider::Warp {"decoded"} else {"converter"},"instance":"ctx-asr-sqlite-batch6-2","source":{"format":provider.format()},"lossy":losses},
        "profiles":["core","coding","lineage","usage"],"contentLevel":"full","authority":if provider==Provider::Warp {"remote_cache"} else {"local"},"workspace":{"roots":[{"id":"unknown","path":null}]}})
}
pub fn capture_header(
    header: &mut Value,
    provider: Provider,
    row: &Value,
    namespace: Option<&str>,
) {
    header["observedAt"] =
        json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true));
    if provider == Provider::Goose {
        header["namespace"] = json!(namespace);
    }
    if let Some(parent) = row["parent_session_id"].as_str().filter(|s| !s.is_empty()) {
        let reference = if provider == Provider::Goose {
            let ns = session_ref(provider, namespace.unwrap_or_default());
            let parent = session_ref(provider, parent);
            format!(
                "{ns}:{}",
                parent.split_once(':').map(|(_, s)| s).unwrap_or_default()
            )
        } else {
            session_ref(provider, parent)
        };
        header["lineage"] = json!({"relatedUnknown":{"session":reference,"evidence":"native_field"},"inherits":{"mode":"unknown","evidence":"native_field"}});
    }
    if provider == Provider::Warp {
        if let Some(data) = row["conversation_data"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
        {
            if let Some(parent) = data["parent_conversation_id"]
                .as_str()
                .filter(|s| !s.is_empty())
            {
                header["lineage"] = json!({"relatedUnknown":{"session":session_ref(provider,parent),"evidence":"native_field"}});
            }
        }
    }
    // Creation time belongs to the session. Current cwd/model/title do not describe its start.
    if provider == Provider::Crush {
        if let Some(ts) = super::common::unix_time(&row["created_at"], true) {
            header["startedAt"] = json!(ts);
        }
    } else if provider == Provider::Goose {
        if let Some(ts) = row["created_at"]
            .as_str()
            .and_then(crate::records::timestamp)
        {
            header["startedAt"] = json!(ts);
        }
    }
    header["_meta"]["rs.ctx/serialization"] = json!({"transcript":match provider {
            Provider::Crush => "messages: created_at, rowid",
            Provider::Goose => "messages: created_timestamp, id",
            Provider::Lingma => "chat_record: rowid",
            Provider::Warp => "task_id binary order, then repeated message field order; cross-task chronology unknown",
            _ => "unknown",
        },"sideLogs":"separate table order, no timestamp merge","currentState":"capture tail"});
}
