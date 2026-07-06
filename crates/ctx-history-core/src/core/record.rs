#[allow(unused_imports)]
use super::*;

text_enum! {
    pub enum HistoryRecordStatus {
        Open => "open",
        Active => "active",
        Completed => "completed",
        Abandoned => "abandoned",
        Archived => "archived",
    }
    default Open
}

impl fmt::Display for HistoryRecordLinkTargetType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for HistoryRecordLinkTargetType {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HistoryRecordLinkTargetType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistoryRecordLinkType {
    Produced,
    Touched,
    #[default]
    References,
    LikelyRelated,
}

impl HistoryRecordLinkType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Produced => "produced",
            Self::Touched => "touched",
            Self::References => "references",
            Self::LikelyRelated => "likely_related",
        }
    }

    pub fn variants() -> &'static [&'static str] {
        &["produced", "touched", "references", "likely_related"]
    }
}

impl fmt::Display for HistoryRecordLinkType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HistoryRecordLinkType {
    type Err = CoreError;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "produced" => Ok(Self::Produced),
            "touched" => Ok(Self::Touched),
            "references" => Ok(Self::References),
            "likely_related" => Ok(Self::LikelyRelated),
            _ => Err(CoreError::InvalidEnumValue {
                enum_name: "HistoryRecordLinkType",
                value: value.to_owned(),
            }),
        }
    }
}

impl Serialize for HistoryRecordLinkType {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HistoryRecordLinkType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

text_enum! {
    pub enum RecordEdgeType {
        Continues => "continues",
        Duplicates => "duplicates",
        Blocks => "blocks",
        Related => "related",
        Supersedes => "supersedes",
        SplitFrom => "split_from",
    }
    default Related
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub kind: String,
    pub workspace: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl HistoryRecord {
    pub fn new(
        title: impl Into<String>,
        body: impl Into<String>,
        tags: Vec<String>,
        kind: impl Into<String>,
        workspace: Option<String>,
    ) -> Self {
        let now = utc_now();
        Self {
            id: new_id(),
            title: title.into(),
            body: body.into(),
            tags,
            kind: kind.into(),
            workspace,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryRecordMetadata {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub status: HistoryRecordStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_vcs_workspace_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: Uuid,
    #[serde(
        default,
        rename = "history_record_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub history_record_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    pub run_type: RunType,
    pub status: RunStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_blob_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_blob_id: Option<Uuid>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryRecordLink {
    pub id: Uuid,
    #[serde(rename = "history_record_id")]
    pub history_record_id: Uuid,
    pub target_type: HistoryRecordLinkTargetType,
    pub target_id: Uuid,
    pub link_type: HistoryRecordLinkType,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CitationReference {
    pub target_type: HistoryRecordLinkTargetType,
    pub target_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub id: Uuid,
    #[serde(
        default,
        rename = "history_record_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub history_record_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    pub kind: SummaryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_or_source: Option<String>,
    pub text: String,
    #[serde(default)]
    pub citations: Vec<CitationReference>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileTouched {
    pub id: Uuid,
    #[serde(
        default,
        rename = "history_record_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub history_record_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs_workspace_id: Option<Uuid>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_kind: Option<FileChangeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_count_delta: Option<i64>,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRecordTag {
    #[serde(rename = "history_record_id")]
    pub history_record_id: Uuid,
    pub tag_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(default)]
    pub confidence: Confidence,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordEdge {
    pub id: Uuid,
    pub from_record_id: Uuid,
    pub to_record_id: Uuid,
    pub edge_type: RecordEdgeType,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}
