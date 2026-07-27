use std::fmt;

#[cfg(test)]
use std::io;

use ctx_history_core::{EventRole, EventType};
use serde::{
    de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor},
    Deserialize, Serialize,
};

use crate::PROVIDER_MAX_TEXT_CHARS;

#[cfg(test)]
use super::checkpoint::CursorCheckpoint;
#[cfg(test)]
use crate::Result;

const MAX_CURSOR_ATOM_CHARS: usize = 512;
const MAX_CURSOR_PATH_CHARS: usize = 4_096;
const MAX_CURSOR_INPUT_PATHS: usize = 32;
pub(super) const CURSOR_REJECTION_SAMPLE_LIMIT: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CursorRejectionKind {
    MalformedJson,
    Oversized,
    UnsupportedShape,
}

impl CursorRejectionKind {
    pub(super) fn proof_marker(self) -> &'static [u8] {
        match self {
            Self::MalformedJson => b"malformed-json",
            Self::Oversized => b"oversized",
            Self::UnsupportedShape => b"unsupported-shape",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorRecordRejection {
    pub(crate) physical_line: u64,
    pub(crate) kind: CursorRejectionKind,
    pub(crate) observed_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct CursorRejectionSummary {
    pub(super) total: u64,
    pub(super) samples: Vec<CursorRecordRejection>,
}

impl CursorRejectionSummary {
    fn record(&mut self, rejection: CursorRecordRejection) {
        self.total = self.total.saturating_add(1);
        if self.samples.len() < CURSOR_REJECTION_SAMPLE_LIMIT {
            self.samples.push(rejection);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) enum CursorSafePart {
    BodyFree {
        event_type: EventType,
        role: EventRole,
    },
    Text {
        event_type: EventType,
        role: EventRole,
        text: String,
    },
    ToolUse {
        role: EventRole,
        call_id: Option<String>,
        tool_name: Option<String>,
        input_paths: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct CursorSanitizedRecord {
    pub(super) semantic_ordinal: u64,
    pub(super) timestamp: Option<String>,
    pub(super) role: EventRole,
    pub(super) event: Option<String>,
    pub(super) record_type: Option<String>,
    pub(super) status: Option<String>,
    pub(super) result_blocks: u32,
    pub(super) parts: Vec<CursorSafePart>,
}

mod classification;
mod output;
mod stream;

use classification::{
    classify_cursor_line, CursorBlockKind, CursorContentLocation, CursorLineClassification,
    CursorRecordAdmission,
};
pub(crate) use output::{
    scan_cursor_output_pages, CursorOutputFact, CursorOutputPage, CursorOutputScanOutcome,
};

#[cfg(test)]
pub(super) use stream::CursorParsedGeneration;
pub(super) use stream::{
    scan_cursor_reader, CursorParserOutcome, CursorParserPlan, CursorParserStats,
};

fn decode_sanitized_record(
    bytes: &[u8],
    semantic_ordinal: u64,
    classification: &CursorLineClassification,
) -> serde_json::Result<CursorSanitizedRecord> {
    if classification.admission == CursorRecordAdmission::Excluded {
        return Ok(CursorSanitizedRecord {
            semantic_ordinal,
            timestamp: classification.timestamp.clone(),
            role: EventRole::Unknown,
            event: classification.event.clone(),
            record_type: classification.record_type.clone(),
            status: classification.status.clone(),
            result_blocks: classification.result_blocks,
            parts: Vec::new(),
        });
    }
    let has_retained_blocks = classification
        .block_kinds
        .iter()
        .any(|kind| matches!(kind, CursorBlockKind::Text | CursorBlockKind::ToolUse));
    let mut parts = if has_retained_blocks {
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let parts = CursorRetainedSeed { classification }.deserialize(&mut deserializer)?;
        deserializer.end()?;
        parts
    } else {
        Vec::new()
    };
    let role = cursor_role(classification.role.as_deref());
    if parts.is_empty() && classification.result_blocks == 0 {
        parts.push(CursorSafePart::BodyFree {
            event_type: cursor_text_event_type(classification),
            role,
        });
    }
    Ok(CursorSanitizedRecord {
        semantic_ordinal,
        timestamp: classification.timestamp.clone(),
        role,
        event: classification.event.clone(),
        record_type: classification.record_type.clone(),
        status: classification.status.clone(),
        result_blocks: classification.result_blocks,
        parts,
    })
}

fn cursor_text_event_type(classification: &CursorLineClassification) -> EventType {
    match classification.admission {
        CursorRecordAdmission::UserMessage | CursorRecordAdmission::AssistantMessage => {
            EventType::Message
        }
        CursorRecordAdmission::TurnEndedSummary => EventType::Summary,
        CursorRecordAdmission::Excluded => EventType::Notice,
    }
}

fn cursor_role(role: Option<&str>) -> EventRole {
    match role {
        Some("user") => EventRole::User,
        Some("assistant") => EventRole::Assistant,
        Some("system") => EventRole::System,
        Some("tool") => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

struct CursorRetainedSeed<'a> {
    classification: &'a CursorLineClassification,
}

impl<'de> DeserializeSeed<'de> for CursorRetainedSeed<'_> {
    type Value = Vec<CursorSafePart>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(CursorRetainedVisitor {
            classification: self.classification,
        })
    }
}

struct CursorRetainedVisitor<'a> {
    classification: &'a CursorLineClassification,
}

impl<'de> Visitor<'de> for CursorRetainedVisitor<'_> {
    type Value = Vec<CursorSafePart>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a classified Cursor transcript object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut parts = Vec::new();
        while let Some(field) = map.next_key::<String>()? {
            match (field.as_str(), self.classification.location) {
                ("message", CursorContentLocation::Message) => {
                    parts = map.next_value_seed(CursorMessageRetainedSeed {
                        kinds: &self.classification.block_kinds,
                        classification: self.classification,
                    })?;
                }
                ("content", CursorContentLocation::TopLevel) => {
                    parts = map.next_value_seed(CursorContentRetainedSeed {
                        kinds: &self.classification.block_kinds,
                        classification: self.classification,
                    })?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(parts)
    }
}

struct CursorMessageRetainedSeed<'a> {
    kinds: &'a [CursorBlockKind],
    classification: &'a CursorLineClassification,
}

impl<'de> DeserializeSeed<'de> for CursorMessageRetainedSeed<'_> {
    type Value = Vec<CursorSafePart>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(CursorMessageRetainedVisitor {
            kinds: self.kinds,
            classification: self.classification,
        })
    }
}

struct CursorMessageRetainedVisitor<'a> {
    kinds: &'a [CursorBlockKind],
    classification: &'a CursorLineClassification,
}

impl<'de> Visitor<'de> for CursorMessageRetainedVisitor<'_> {
    type Value = Vec<CursorSafePart>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor message object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut parts = Vec::new();
        while let Some(field) = map.next_key::<String>()? {
            if field == "content" {
                parts = map.next_value_seed(CursorContentRetainedSeed {
                    kinds: self.kinds,
                    classification: self.classification,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(parts)
    }
}

struct CursorContentRetainedSeed<'a> {
    kinds: &'a [CursorBlockKind],
    classification: &'a CursorLineClassification,
}

impl<'de> DeserializeSeed<'de> for CursorContentRetainedSeed<'_> {
    type Value = Vec<CursorSafePart>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(CursorContentRetainedVisitor {
            kinds: self.kinds,
            classification: self.classification,
        })
    }
}

struct CursorContentRetainedVisitor<'a> {
    kinds: &'a [CursorBlockKind],
    classification: &'a CursorLineClassification,
}

impl<'de> Visitor<'de> for CursorContentRetainedVisitor<'_> {
    type Value = Vec<CursorSafePart>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the previously classified Cursor content array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut parts = Vec::new();
        for kind in self.kinds {
            let Some(part) = sequence.next_element_seed(CursorBlockRetainedSeed {
                kind: *kind,
                classification: self.classification,
            })?
            else {
                return Err(de::Error::custom(
                    "Cursor content changed between classification and decoding",
                ));
            };
            if let Some(part) = part {
                parts.push(part);
            }
        }
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(parts)
    }
}

struct CursorBlockRetainedSeed<'a> {
    kind: CursorBlockKind,
    classification: &'a CursorLineClassification,
}

impl<'de> DeserializeSeed<'de> for CursorBlockRetainedSeed<'_> {
    type Value = Option<CursorSafePart>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        match self.kind {
            CursorBlockKind::Text => deserializer.deserialize_map(CursorTextBlockVisitor {
                classification: self.classification,
            }),
            CursorBlockKind::ToolUse => deserializer.deserialize_map(CursorToolUseBlockVisitor {
                classification: self.classification,
            }),
            CursorBlockKind::Excluded => {
                IgnoredAny::deserialize(deserializer)?;
                Ok(None)
            }
        }
    }
}

struct CursorTextBlockVisitor<'a> {
    classification: &'a CursorLineClassification,
}

impl<'de> Visitor<'de> for CursorTextBlockVisitor<'_> {
    type Value = Option<CursorSafePart>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor text content block")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut text = None;
        while let Some(field) = map.next_key::<String>()? {
            if field == "text" {
                text = Some(map.next_value_seed(BoundedStringSeed {
                    max_chars: PROVIDER_MAX_TEXT_CHARS,
                })?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(Some(CursorSafePart::Text {
            event_type: cursor_text_event_type(self.classification),
            role: cursor_role(self.classification.role.as_deref()),
            text: text.unwrap_or_default(),
        }))
    }
}

struct CursorToolUseBlockVisitor<'a> {
    classification: &'a CursorLineClassification,
}

impl<'de> Visitor<'de> for CursorToolUseBlockVisitor<'_> {
    type Value = Option<CursorSafePart>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor tool_use content block")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut call_id = None;
        let mut tool_name = None;
        let mut input_paths = Vec::new();
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "id" => {
                    call_id = Some(map.next_value_seed(BoundedStringSeed {
                        max_chars: MAX_CURSOR_ATOM_CHARS,
                    })?);
                }
                "name" => {
                    tool_name = Some(map.next_value_seed(BoundedStringSeed {
                        max_chars: MAX_CURSOR_ATOM_CHARS,
                    })?);
                }
                "input" => {
                    input_paths = map.next_value_seed(CursorToolInputSeed)?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Some(CursorSafePart::ToolUse {
            role: match cursor_role(self.classification.role.as_deref()) {
                EventRole::Unknown => EventRole::Assistant,
                role => role,
            },
            call_id,
            tool_name,
            input_paths,
        }))
    }
}

struct CursorToolInputSeed;

impl<'de> DeserializeSeed<'de> for CursorToolInputSeed {
    type Value = Vec<String>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CursorToolInputVisitor)
    }
}

struct CursorToolInputVisitor;

impl<'de> Visitor<'de> for CursorToolInputVisitor {
    type Value = Vec<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cursor tool input object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut paths = Vec::new();
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "path" | "file_path" | "filePath" if paths.len() < MAX_CURSOR_INPUT_PATHS => {
                    paths.push(map.next_value_seed(BoundedStringSeed {
                        max_chars: MAX_CURSOR_PATH_CHARS,
                    })?);
                }
                "paths" if paths.len() < MAX_CURSOR_INPUT_PATHS => {
                    let mut decoded = map.next_value_seed(CursorPathsSeed)?;
                    let remaining = MAX_CURSOR_INPUT_PATHS.saturating_sub(paths.len());
                    paths.extend(decoded.drain(..decoded.len().min(remaining)));
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(paths)
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Vec::new())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Vec::new())
    }
}

struct CursorPathsSeed;

impl<'de> DeserializeSeed<'de> for CursorPathsSeed {
    type Value = Vec<String>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(CursorPathsVisitor)
    }
}

struct CursorPathsVisitor;

impl<'de> Visitor<'de> for CursorPathsVisitor {
    type Value = Vec<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded array of Cursor input paths")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut paths = Vec::new();
        while paths.len() < MAX_CURSOR_INPUT_PATHS {
            let Some(path) = sequence.next_element_seed(BoundedStringSeed {
                max_chars: MAX_CURSOR_PATH_CHARS,
            })?
            else {
                return Ok(paths);
            };
            paths.push(path);
        }
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(paths)
    }
}

struct BoundedStringSeed {
    max_chars: usize,
}

impl<'de> DeserializeSeed<'de> for BoundedStringSeed {
    type Value = String;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(BoundedStringVisitor {
            max_chars: self.max_chars,
        })
    }
}

struct BoundedStringVisitor {
    max_chars: usize,
}

impl Visitor<'_> for BoundedStringVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded string")
    }

    fn visit_borrowed_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value.chars().take(self.max_chars).collect())
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value.chars().take(self.max_chars).collect())
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.chars().count() <= self.max_chars {
            Ok(value)
        } else {
            Ok(value.chars().take(self.max_chars).collect())
        }
    }
}

#[cfg(test)]
struct CursorCollectingPublicationSink {
    events: Vec<super::projection::CursorNativeEvent>,
}

#[cfg(test)]
impl super::projection::CursorPublicationSink for CursorCollectingPublicationSink {
    fn begin_cursor_publication(&mut self) -> Result<()> {
        self.events.clear();
        Ok(())
    }

    fn stage_cursor_page(&mut self, page: super::projection::CursorPublicationPage) -> Result<()> {
        self.events.extend(page.events);
        Ok(())
    }

    fn abort_cursor_publication(&mut self) {
        self.events.clear();
    }

    fn commit_cursor_publication(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn scan_cursor_bytes_into_sink(
    bytes: &[u8],
    checkpoint: Option<&CursorCheckpoint>,
    max_line_bytes: usize,
    sink: &mut dyn super::projection::CursorPublicationSink,
) -> Result<CursorParserOutcome> {
    sink.begin_cursor_publication()?;
    let mut reader = io::BufReader::new(bytes);
    let plan = checkpoint.map_or(CursorParserPlan::FullSnapshot, |checkpoint| {
        CursorParserPlan::VerifyPrefixAndResume(checkpoint)
    });
    let result = stream::scan_cursor_reader_with_limit(&mut reader, plan, sink, max_line_bytes);
    match result {
        Ok(outcome) => match sink.commit_cursor_publication() {
            Ok(()) => Ok(outcome),
            Err(error) => {
                sink.abort_cursor_publication();
                Err(error)
            }
        },
        Err(error) => {
            sink.abort_cursor_publication();
            Err(error)
        }
    }
}

#[cfg(test)]
pub(super) fn scan_cursor_bytes_with_limit(
    bytes: &[u8],
    checkpoint: Option<&CursorCheckpoint>,
    max_line_bytes: usize,
) -> Result<CursorParserOutcome> {
    let mut sink = CursorCollectingPublicationSink { events: Vec::new() };
    let outcome = scan_cursor_bytes_into_sink(bytes, checkpoint, max_line_bytes, &mut sink)?;
    Ok(match outcome {
        CursorParserOutcome::Parsed(mut parsed) => {
            // Preserve the historical test helper contract. The production
            // reader never owns this vector; only this collecting wrapper does.
            parsed.events = sink.events;
            CursorParserOutcome::Parsed(parsed)
        }
        CursorParserOutcome::PrefixMismatch(stats) => CursorParserOutcome::PrefixMismatch(stats),
    })
}
