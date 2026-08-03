use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::CoreError;

text_enum! {
    pub enum Confidence {
        Explicit => "explicit",
        High => "high",
        Medium => "medium",
        Low => "low",
        Unknown => "unknown",
    }
    default Unknown
}

text_enum! {
    pub enum Fidelity {
        Full => "full",
        Partial => "partial",
        Imported => "imported",
        Inferred => "inferred",
        SummaryOnly => "summary_only",
    }
    default Partial
}

text_enum! {
    pub enum AgentType {
        Primary => "primary",
        Subagent => "subagent",
        AgentTeamMember => "agent_team_member",
        Reviewer => "reviewer",
        Implementer => "implementer",
        Unknown => "unknown",
    }
    default Unknown
}

text_enum! {
    pub enum SessionStatus {
        Started => "started",
        Active => "active",
        Idle => "idle",
        Completed => "completed",
        Failed => "failed",
        Interrupted => "interrupted",
        Imported => "imported",
    }
    default Started
}

text_enum! {
    pub enum SessionEdgeType {
        ParentChild => "parent_child",
        Delegated => "delegated",
        Reviewed => "reviewed",
        Spawned => "spawned",
        ResumedFrom => "resumed_from",
        ImportedRelated => "imported_related",
    }
    default ImportedRelated
}

text_enum! {
    pub enum EventType {
        Message => "message",
        ToolCall => "tool_call",
        ToolOutput => "tool_output",
        CommandStarted => "command_started",
        CommandOutput => "command_output",
        CommandFinished => "command_finished",
        FileTouched => "file_touched",
        VcsChange => "vcs_change",
        Artifact => "artifact",
        Summary => "summary",
        Notice => "notice",
    }
    default Notice
}

text_enum! {
    pub enum EventRole {
        User => "user",
        Assistant => "assistant",
        System => "system",
        Tool => "tool",
        Unknown => "unknown",
    }
    default Unknown
}

text_enum! {
    pub enum ArtifactKind {
        Transcript => "transcript",
        Stdout => "stdout",
        Stderr => "stderr",
        Screenshot => "screenshot",
        Report => "report",
        Diff => "diff",
        FileSnapshot => "file_snapshot",
        Json => "json",
        Markdown => "markdown",
        Binary => "binary",
    }
    default Binary
}

text_enum! {
    pub enum FileChangeKind {
        Read => "read",
        Created => "created",
        Modified => "modified",
        Deleted => "deleted",
        Renamed => "renamed",
        Unknown => "unknown",
    }
    default Unknown
}
