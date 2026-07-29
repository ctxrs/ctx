use super::*;

#[derive(Default)]
pub(super) struct ZedResultWire {
    pub(super) is_error: Option<bool>,
    pub(super) content: DiscardedJson,
    pub(super) output: Option<ZedResultOutputWire>,
    pub(super) shape_is_unambiguous: bool,
}

impl<'de> Deserialize<'de> for ZedResultWire {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ZedResultVisitor)
    }
}

struct ZedResultVisitor;

impl<'de> Visitor<'de> for ZedResultVisitor {
    type Value = ZedResultWire;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any Zed tool-result shape")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut result = ZedResultWire {
            shape_is_unambiguous: true,
            ..ZedResultWire::default()
        };
        let mut saw_is_error = false;
        let mut saw_content = false;
        let mut saw_output = false;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "is_error" => {
                    let value = map.next_value::<TolerantBool>()?.0;
                    if saw_is_error || value.is_none() {
                        result.shape_is_unambiguous = false;
                    } else {
                        result.is_error = value;
                    }
                    saw_is_error = true;
                }
                "content" => {
                    let value = map.next_value::<DiscardedJson>()?;
                    result.content.string_bytes = result
                        .content
                        .string_bytes
                        .saturating_add(value.string_bytes);
                    if saw_content {
                        result.shape_is_unambiguous = false;
                    }
                    saw_content = true;
                }
                "output" => {
                    let parsed = map.next_value::<TolerantResultOutput>()?;
                    if saw_output || !parsed.valid {
                        result.shape_is_unambiguous = false;
                    } else {
                        result.output = parsed.value;
                    }
                    saw_output = true;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(result)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(ZedResultWire::default())
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(ZedResultWire::default())
    }
}

pub(super) struct ZedResultOutputWire;

struct TolerantResultOutput {
    value: Option<ZedResultOutputWire>,
    valid: bool,
}

impl<'de> Deserialize<'de> for TolerantResultOutput {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(TolerantResultOutputVisitor)
    }
}

struct TolerantResultOutputVisitor;

impl<'de> Visitor<'de> for TolerantResultOutputVisitor {
    type Value = TolerantResultOutput;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Zed result-output object or an ignored value")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut saw_status = false;
        let mut valid = true;
        while let Some(key) = map.next_key::<String>()? {
            if key == "status" {
                let parsed = map.next_value::<TolerantString>()?.0;
                if saw_status || parsed.is_none() {
                    valid = false;
                }
                saw_status = true;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(TolerantResultOutput {
            value: Some(ZedResultOutputWire),
            valid,
        })
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(TolerantResultOutput {
            value: None,
            valid: false,
        })
    }
}

struct TolerantBool(Option<bool>);

impl<'de> Deserialize<'de> for TolerantBool {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(TolerantBoolVisitor)
    }
}

struct TolerantBoolVisitor;

impl<'de> Visitor<'de> for TolerantBoolVisitor {
    type Value = TolerantBool;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a boolean or an ignored value")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(Some(value)))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(TolerantBool(None))
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(TolerantBool(None))
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(TolerantBool(None))
    }
}

struct TolerantString(Option<String>);

impl<'de> Deserialize<'de> for TolerantString {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(TolerantStringVisitor)
    }
}

struct TolerantStringVisitor;

impl<'de> Visitor<'de> for TolerantStringVisitor {
    type Value = TolerantString;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a string or an ignored value")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(Some(value.to_owned())))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(Some(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(Some(value)))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(TolerantString(None))
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(TolerantString(None))
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(None))
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(None))
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(None))
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(None))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(None))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(TolerantString(None))
    }
}

pub(super) fn collect_safe_touches(value: &Value, touches: &mut BTreeSet<String>) {
    if touches.len() >= ZED_MAX_SAFE_TOUCHES_PER_EVENT {
        return;
    }
    match value {
        Value::Array(values) => {
            for value in values {
                collect_safe_touches(value, touches);
                if touches.len() >= ZED_MAX_SAFE_TOUCHES_PER_EVENT {
                    break;
                }
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(key.as_str(), "path" | "file_path" | "file")
                    && value.as_str().is_some_and(|path| {
                        !path.trim().is_empty() && path.len() <= ZED_MAX_SAFE_TOUCH_BYTES
                    })
                {
                    if let Some(path) = value.as_str() {
                        touches.insert(path.to_owned());
                    }
                } else {
                    collect_safe_touches(value, touches);
                }
                if touches.len() >= ZED_MAX_SAFE_TOUCHES_PER_EVENT {
                    break;
                }
            }
        }
        _ => {}
    }
}

pub(super) fn push_nonempty(parts: &mut Vec<String>, value: String) {
    if !value.trim().is_empty() {
        parts.push(value);
    }
}

pub(super) fn nonempty_owned(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

#[derive(Deserialize)]
pub(super) struct ZedThreadWire {
    #[serde(default)]
    pub(super) version: Option<String>,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) updated_at: Option<String>,
    #[serde(default)]
    pub(super) messages: Option<ZedValidatedMessages>,
}

pub(super) struct ZedValidatedMessages {
    pub(super) count: usize,
}

impl<'de> Deserialize<'de> for ZedValidatedMessages {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(ZedValidatedMessagesVisitor)
    }
}

struct ZedValidatedMessagesVisitor;

impl<'de> Visitor<'de> for ZedValidatedMessagesVisitor {
    type Value = ZedValidatedMessages;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Zed message sequence")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut count = 0_usize;
        while sequence.next_element::<ZedMessageWire>()?.is_some() {
            count = count.saturating_add(1);
        }
        Ok(ZedValidatedMessages { count })
    }
}

pub(super) enum ZedMessageWire {
    User(ZedUserWire),
    Agent(ZedAgentWire),
    Compaction(Option<String>),
    Resume,
    Unknown(String),
}

impl<'de> Deserialize<'de> for ZedMessageWire {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ZedMessageVisitor)
    }
}

struct ZedMessageVisitor;

impl<'de> Visitor<'de> for ZedMessageVisitor {
    type Value = ZedMessageWire;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Zed externally tagged message")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(if value == "Resume" {
            ZedMessageWire::Resume
        } else {
            ZedMessageWire::Unknown(value.to_owned())
        })
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(if value == "Resume" {
            ZedMessageWire::Resume
        } else {
            ZedMessageWire::Unknown(value.to_owned())
        })
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(if value == "Resume" {
            ZedMessageWire::Resume
        } else {
            ZedMessageWire::Unknown(value)
        })
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let kind = map
            .next_key::<String>()?
            .ok_or_else(|| serde::de::Error::custom("Zed message tag is empty"))?;
        let message = match kind.as_str() {
            "User" => ZedMessageWire::User(map.next_value()?),
            "Agent" => ZedMessageWire::Agent(map.next_value()?),
            "Compaction" => {
                let value: ZedCompactionWire = map.next_value()?;
                ZedMessageWire::Compaction(value.summary)
            }
            "Resume" => {
                map.next_value::<IgnoredAny>()?;
                ZedMessageWire::Resume
            }
            _ => {
                map.next_value::<IgnoredAny>()?;
                ZedMessageWire::Unknown(kind)
            }
        };
        if map.next_key::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(
                "Zed message must contain exactly one external tag",
            ));
        }
        Ok(message)
    }
}

#[derive(Deserialize)]
pub(super) struct ZedUserWire {
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) content: Vec<ZedContentWire>,
}

#[derive(Deserialize)]
pub(super) struct ZedAgentWire {
    #[serde(default)]
    pub(super) content: Vec<ZedContentWire>,
    #[serde(default, rename = "tool_results")]
    pub(super) _tool_results: ZedToolResultsWire,
}

#[derive(Default)]
pub(super) struct ZedToolResultsWire;

impl<'de> Deserialize<'de> for ZedToolResultsWire {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ZedToolResultsVisitor)
    }
}

struct ZedToolResultsVisitor;

impl<'de> Visitor<'de> for ZedToolResultsVisitor {
    type Value = ZedToolResultsWire;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Zed tool-results object or discarded output-only evidence")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, ZedResultWire>()?.is_some() {}
        Ok(ZedToolResultsWire)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(ZedToolResultsWire)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire)
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire)
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire)
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire)
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire)
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire)
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire)
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire)
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(ZedToolResultsWire)
    }
}

#[derive(Deserialize)]
struct ZedCompactionWire {
    #[serde(default, rename = "Summary")]
    summary: Option<String>,
}

pub(super) enum ZedContentWire {
    Text(String),
    Thinking(String),
    RedactedThinking,
    ToolUse(ZedToolUseWire),
    ToolResult,
    Mention(Option<String>),
    Image,
    Unknown(String),
}

impl<'de> Deserialize<'de> for ZedContentWire {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ZedContentVisitor)
    }
}

struct ZedContentVisitor;

impl<'de> Visitor<'de> for ZedContentVisitor {
    type Value = ZedContentWire;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Zed externally tagged content value")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let kind = map
            .next_key::<String>()?
            .ok_or_else(|| serde::de::Error::custom("Zed content tag is empty"))?;
        let content = match kind.as_str() {
            "Text" => ZedContentWire::Text(map.next_value()?),
            "Thinking" => {
                let value: ZedThinkingWire = map.next_value()?;
                ZedContentWire::Thinking(value.text.unwrap_or_default())
            }
            "RedactedThinking" => {
                map.next_value::<IgnoredAny>()?;
                ZedContentWire::RedactedThinking
            }
            "ToolUse" => ZedContentWire::ToolUse(map.next_value()?),
            "ToolResult" => {
                map.next_value::<ZedResultWire>()?;
                ZedContentWire::ToolResult
            }
            "Mention" => {
                let value: ZedMentionWire = map.next_value()?;
                ZedContentWire::Mention(value.content)
            }
            "Image" => {
                map.next_value::<IgnoredAny>()?;
                ZedContentWire::Image
            }
            _ => {
                map.next_value::<IgnoredAny>()?;
                ZedContentWire::Unknown(kind)
            }
        };
        if map.next_key::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(
                "Zed content must contain exactly one external tag",
            ));
        }
        Ok(content)
    }
}

#[derive(Deserialize)]
struct ZedThinkingWire {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct ZedMentionWire {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct ZedToolUseWire {
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) input: Option<Value>,
    #[serde(default)]
    pub(super) raw_input: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct DiscardedJson {
    pub(super) string_bytes: u64,
}

impl<'de> Deserialize<'de> for DiscardedJson {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DiscardedJsonVisitor)
    }
}

struct DiscardedJsonVisitor;

impl<'de> Visitor<'de> for DiscardedJsonVisitor {
    type Value = DiscardedJson;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value to discard")
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson::default())
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson {
            string_bytes: u64::try_from(value.len()).unwrap_or(u64::MAX),
        })
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson {
            string_bytes: u64::try_from(value.len()).unwrap_or(u64::MAX),
        })
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(DiscardedJson {
            string_bytes: u64::try_from(value.len()).unwrap_or(u64::MAX),
        })
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut string_bytes = 0_u64;
        while let Some(value) = sequence.next_element::<DiscardedJson>()? {
            string_bytes = string_bytes.saturating_add(value.string_bytes);
        }
        Ok(DiscardedJson { string_bytes })
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut string_bytes = 0_u64;
        while let Some((_key, value)) = map.next_entry::<IgnoredAny, DiscardedJson>()? {
            string_bytes = string_bytes.saturating_add(value.string_bytes);
        }
        Ok(DiscardedJson { string_bytes })
    }
}
