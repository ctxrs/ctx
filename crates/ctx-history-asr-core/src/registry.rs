//! Reserved native destinations from ASR validator-v0.1.2 registry.json.
use crate::Provider;
use serde_json::{json, Value};

pub fn reserved(domain: &str, key: &str) -> bool {
    if matches!(key, "remainingFields" | "jsonText" | "unassigned") {
        return true;
    }
    match domain {
        "com.google.antigravity" => matches!(
            key,
            "created_at" | "source" | "status" | "stepType" | "step_index"
        ),
        "app.astrbot" => matches!(key, "persona" | "route"),
        "com.anthropic.claude-code" => matches!(
            key,
            "advisorModel"
                | "afkTimeoutMs"
                | "agentColor"
                | "agentMeta"
                | "agentName"
                | "agentStats"
                | "annotations"
                | "apiRequest"
                | "appliedEdits"
                | "attributedTo"
                | "compactMetadata"
                | "costState"
                | "dangerouslyDisableSandbox"
                | "goalStatus"
                | "inferenceGeo"
                | "moreFilesCount"
                | "origin"
                | "permissionReasonType"
                | "previews"
                | "promptSource"
                | "queue"
                | "quotaLimits"
                | "requestId"
                | "serverToolUse"
                | "slug"
                | "speed"
                | "stopDetails"
                | "thinkingDurationMs"
                | "toolsDelta"
                | "turnCompanion"
                | "turnPosition"
                | "wireToolInputs"
        ),
        "bot.cline" => matches!(key, "task_metadata"),
        "com.tencent.codebuddy" => matches!(key, "catalogReference"),
        "com.openai.codex" => matches!(
            key,
            "abortReason"
                | "agent"
                | "autoApproved"
                | "cellId"
                | "command"
                | "compaction"
                | "connector"
                | "creator"
                | "durationMs"
                | "encryptedFunctionArgs"
                | "identityRecovery"
                | "mcpAttribution"
                | "memoryCitation"
                | "parentTurnId"
                | "position"
                | "rateLimits"
                | "readAt"
                | "section"
                | "senderUserMessages"
                | "thread"
                | "timeToFirstTokenMs"
                | "tokenLimit"
                | "traceId"
                | "userInputOrder"
        ),
        "dev.continue" => matches!(key, "promptLog"),
        "com.github.copilot-cli" => matches!(key, "message" | "partCount"),
        "sh.charm.crush" => matches!(key, "file" | "messageId" | "source"),
        "com.langchain.deepagents" => matches!(key, "call" | "usage"),
        "com.deepseek.harness" => matches!(
            key,
            "attachment" | "diff" | "error" | "messageSource" | "replacementMeter" | "usage"
        ),
        "ai.cognition.devin" => matches!(
            key,
            "call" | "diff" | "replacementParent" | "state" | "usage"
        ),
        "ai.factory.droid" => matches!(key, "details" | "message"),
        "com.firebender" => matches!(key, "call" | "state"),
        "dev.forgecode" => matches!(key, "call" | "state" | "usage"),
        "com.google.gemini-cli" => matches!(
            key,
            "functionParts" | "rewindTo" | "subject" | "timestamp" | "usage"
        ),
        "xyz.block.goose" => matches!(key, "messageId" | "source"),
        "ai.x.grok-build" => matches!(
            key,
            "chunkCount"
                | "chunks"
                | "content"
                | "diff"
                | "eventMeta"
                | "toolContent"
                | "updateMeta"
        ),
        "com.nousresearch.hermes" => matches!(key, "modelUsage" | "origin" | "threadId"),
        "com.jetbrains.junie" => matches!(key, "metadata" | "nativeStep"),
        "ai.kilocode" => matches!(key, "part" | "step"),
        "com.moonshot.kimi-code" => matches!(key, "lane" | "partCount"),
        "dev.kiro.cli" => matches!(
            key,
            "assistant" | "requestMetadata" | "session" | "toolResult" | "toolUse" | "user"
        ),
        "com.aliyun.lingma" => matches!(key, "error" | "errorText" | "source"),
        "com.xiaomi.mimocode" => matches!(key, "part" | "step"),
        "ai.mistral.vibe" => matches!(
            key,
            "agentProfile"
                | "config"
                | "contextBoundary"
                | "manualShell"
                | "metadata"
                | "stats"
                | "toolsAvailable"
        ),
        "com.coder.mux" => matches!(key, "compactionMetadata" | "metadata" | "usageSource"),
        "dev.nanoclaw" => matches!(key, "agentGroupId" | "content" | "session"),
        "ai.openclaw" => matches!(
            key,
            "agentId"
                | "appendMode"
                | "async"
                | "compaction"
                | "consumedAt"
                | "consumedBy"
                | "consumerId"
                | "contextUsage"
                | "costComponents"
                | "decisionFacts"
                | "delivery"
                | "executionMode"
                | "expiresAt"
                | "goal"
                | "goalState"
                | "harnessId"
                | "humanMentions"
                | "identity"
                | "media"
                | "message"
                | "mirror"
                | "node"
                | "operationId"
                | "parentToolCallId"
                | "presented"
                | "providerReplay"
                | "queueId"
                | "replyToPreview"
                | "requestedAt"
                | "requesterOrigin"
                | "resolverKind"
                | "risk"
                | "role"
                | "senderIsOwner"
                | "skillAction"
                | "skillChange"
                | "sourceSessions"
                | "standingGrant"
                | "subagent"
                | "submittedBy"
                | "terminalReason"
                | "thoughtSignature"
                | "transcriptVersion"
                | "usageReport"
                | "window"
        ),
        "ai.opencode" => matches!(key, "part" | "step"),
        "dev.all-hands.openhands" => matches!(key, "accumulatedMetrics" | "metrics" | "state"),
        "io.github.badlogic.pi" => matches!(
            key,
            "costComponents" | "diff" | "request" | "textSignature" | "thinkingSignature" | "usage"
        ),
        "com.qoder.cli" => matches!(key, "details" | "message"),
        "io.github.qwenlm.qwen-code" => matches!(key, "systemPayload" | "usage"),
        "com.roocode" => matches!(key, "task_metadata"),
        "dev.exe.shelley" => matches!(key, "openaiReasoning"),
        "codes.t3" => matches!(
            key,
            "attachmentAnswers"
                | "attemptStatus"
                | "backgroundTasks"
                | "contextTransfer"
                | "createdBy"
                | "creationSource"
                | "delegatedCompletion"
                | "dispatch"
                | "fileStats"
                | "goalChecks"
                | "guest"
                | "handoff"
                | "historyOrigin"
                | "inbox"
                | "initialAnswers"
                | "limitRecovery"
                | "nativeTaskRef"
                | "nativeThreadRef"
                | "nativeTurnRef"
                | "notification"
                | "prLink"
                | "prWatch"
                | "previews"
                | "promptCache"
                | "providerUsage"
                | "required"
                | "responseCapability"
                | "run"
                | "runStatus"
                | "scope"
                | "secretRequest"
                | "subagent"
                | "usageStatus"
                | "webhookDelivery"
                | "workflowScript"
        ),
        "com.tabnine.cli" => matches!(key, "parentDirectoryId" | "usageSource"),
        "dev.warp.agent" => matches!(
            key,
            "inputContext"
                | "inputProtobuf"
                | "messageId"
                | "messageSource"
                | "requestId"
                | "resultProtobuf"
                | "shellFinished"
                | "source"
                | "taskId"
        ),
        "dev.zed.agent" => matches!(key, "dirtyPatch" | "settings" | "thread" | "userMessageId"),
        _ => false,
    }
}

pub fn pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Apply registry destinations to ASR objects, without interpreting native payloads.
pub fn normalize(value: &mut Value, provider: Provider) {
    if let Some(meta) = value.get_mut("_meta").and_then(Value::as_object_mut) {
        let keys: Vec<_> = meta.keys().cloned().collect();
        for key in keys {
            let Some((domain, member)) = key.split_once('/') else {
                continue;
            };
            let domain = if domain == "dev.zed" {
                "dev.zed.agent"
            } else {
                domain
            };
            if domain != provider.domain() {
                continue;
            }
            if key.starts_with("dev.zed/") {
                let native = meta.remove(&key).unwrap();
                meta.insert(format!("{domain}/{member}"), native);
                continue;
            }
            if !reserved(domain, member) {
                let native = meta.remove(&key).unwrap();
                let remaining = meta
                    .entry(format!("{domain}/remainingFields"))
                    .or_insert_with(|| json!({}));
                remaining[member] = native;
            }
        }
    }
    match value {
        Value::Array(items) => {
            for item in items {
                normalize(item, provider);
            }
        }
        Value::Object(fields) => {
            for (key, child) in fields {
                if !matches!(key.as_str(), "_meta" | "native" | "raw" | "input" | "data") {
                    normalize(child, provider);
                }
            }
        }
        _ => {}
    }
}
