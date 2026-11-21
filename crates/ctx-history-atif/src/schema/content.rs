//! Multimodal content types for ATIF v1.6+ (images) and v1.8+ (audio).

use std::fmt;

use serde::{
    de::{self, Deserializer},
    ser::SerializeStruct,
    Deserialize, Serialize, Serializer,
};

/// Text-only string or multimodal `ContentPart` array.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.is_empty(),
            Self::Parts(parts) => parts.is_empty(),
        }
    }

    pub fn has_media(&self) -> bool {
        match self {
            Self::Text(_) => false,
            Self::Parts(parts) => parts.iter().any(ContentPart::is_media),
        }
    }

    /// Renders searchable plain text covering text parts plus readable media
    /// placeholders for image and audio references.
    pub fn to_searchable_text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Parts(parts) => {
                let mut segments = Vec::with_capacity(parts.len());
                for part in parts {
                    match part {
                        ContentPart::Text { text } => {
                            if !text.is_empty() {
                                segments.push(text.clone());
                            }
                        }
                        ContentPart::Image { source } => {
                            segments.push(format!(
                                "[image: {} {}]",
                                source.media_type.as_str(),
                                source.path
                            ));
                        }
                        ContentPart::Audio { source } => {
                            if let Some(duration) = source.duration_sec {
                                segments.push(format!(
                                    "[audio: {} {} ({duration}s)]",
                                    source.media_type.as_str(),
                                    source.path
                                ));
                            } else {
                                segments.push(format!(
                                    "[audio: {} {}]",
                                    source.media_type.as_str(),
                                    source.path
                                ));
                            }
                        }
                    }
                }
                segments.join("\n")
            }
        }
    }
}

/// Canonical image MIME types permitted by ATIF v1.6+.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ImageMediaType {
    #[serde(rename = "image/jpeg")]
    Jpeg,
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/gif")]
    Gif,
    #[serde(rename = "image/webp")]
    Webp,
}

impl ImageMediaType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}

impl fmt::Display for ImageMediaType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Image source specification (`ImageSourceSchema`, ATIF v1.6+).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageSource {
    pub media_type: ImageMediaType,
    pub path: String,
}

/// Canonical audio MIME types permitted by ATIF v1.8+.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum AudioMediaType {
    #[serde(rename = "audio/wav")]
    Wav,
    #[serde(rename = "audio/mpeg")]
    Mpeg,
    #[serde(rename = "audio/mp4")]
    Mp4,
    #[serde(rename = "audio/aac")]
    Aac,
    #[serde(rename = "audio/ogg")]
    Ogg,
    #[serde(rename = "audio/flac")]
    Flac,
    #[serde(rename = "audio/webm")]
    Webm,
    #[serde(rename = "audio/aiff")]
    Aiff,
}

impl AudioMediaType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
            Self::Mpeg => "audio/mpeg",
            Self::Mp4 => "audio/mp4",
            Self::Aac => "audio/aac",
            Self::Ogg => "audio/ogg",
            Self::Flac => "audio/flac",
            Self::Webm => "audio/webm",
            Self::Aiff => "audio/aiff",
        }
    }

    /// Parses canonical audio MIME types and normalizes all RFC 0001 v1.8
    /// documented provider aliases (e.g. `audio/mp3` -> `audio/mpeg`).
    pub fn parse_normalized(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => Some(Self::Wav),
            "audio/mpeg" | "audio/mp3" | "audio/mpga" | "audio/x-mpeg" => Some(Self::Mpeg),
            "audio/mp4" | "audio/x-m4a" | "audio/m4a" => Some(Self::Mp4),
            "audio/aac" | "audio/x-aac" => Some(Self::Aac),
            "audio/ogg" => Some(Self::Ogg),
            "audio/flac" | "audio/x-flac" => Some(Self::Flac),
            "audio/webm" => Some(Self::Webm),
            "audio/aiff" | "audio/x-aiff" => Some(Self::Aiff),
            _ => None,
        }
    }
}

impl fmt::Display for AudioMediaType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AudioMediaType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse_normalized(&raw).ok_or_else(|| {
            de::Error::custom(format!(
                "unsupported audio media_type `{raw}`; expected one of audio/wav, audio/mpeg, audio/mp4, audio/aac, audio/ogg, audio/flac, audio/webm, audio/aiff (or documented alias)"
            ))
        })
    }
}

/// Audio source specification (`AudioSourceSchema`, ATIF v1.8+).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioSource {
    pub media_type: AudioMediaType,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_sec: Option<f64>,
}

/// A single multimodal content part (`ContentPartSchema`, ATIF v1.6+ / v1.8+).
///
/// The enum representation enforces ATIF's type/field/source invariants by
/// construction: text parts cannot hold a media source, image parts hold only
/// `ImageSource`, and audio parts hold only `AudioSource`.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentPart {
    Text { text: String },
    Image { source: ImageSource },
    Audio { source: AudioSource },
}

impl ContentPart {
    pub const fn is_media(&self) -> bool {
        matches!(self, Self::Image { .. } | Self::Audio { .. })
    }
}

impl Serialize for ContentPart {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Text { text } => {
                let mut state = serializer.serialize_struct("ContentPart", 2)?;
                state.serialize_field("type", "text")?;
                state.serialize_field("text", text)?;
                state.end()
            }
            Self::Image { source } => {
                let mut state = serializer.serialize_struct("ContentPart", 2)?;
                state.serialize_field("type", "image")?;
                state.serialize_field("source", source)?;
                state.end()
            }
            Self::Audio { source } => {
                let mut state = serializer.serialize_struct("ContentPart", 2)?;
                state.serialize_field("type", "audio")?;
                state.serialize_field("source", source)?;
                state.end()
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContentPart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    source: Option<serde_json::Value>,
}

impl<'de> Deserialize<'de> for ContentPart {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawContentPart::deserialize(deserializer)?;
        match raw.kind.as_str() {
            "text" => {
                if raw.source.is_some() {
                    return Err(de::Error::custom(
                        "'source' field is not allowed when type='text'",
                    ));
                }
                let text = raw.text.ok_or_else(|| {
                    de::Error::custom("'text' field is required when type='text'")
                })?;
                Ok(Self::Text { text })
            }
            "image" => {
                if raw.text.is_some() {
                    return Err(de::Error::custom(
                        "'text' field is not allowed when type='image'",
                    ));
                }
                let source_value = raw.source.ok_or_else(|| {
                    de::Error::custom("'source' field is required when type='image'")
                })?;
                let source: ImageSource =
                    serde_json::from_value(source_value).map_err(|error| {
                        de::Error::custom(format!("type='image' requires an ImageSource: {error}"))
                    })?;
                Ok(Self::Image { source })
            }
            "audio" => {
                if raw.text.is_some() {
                    return Err(de::Error::custom(
                        "'text' field is not allowed when type='audio'",
                    ));
                }
                let source_value = raw.source.ok_or_else(|| {
                    de::Error::custom("'source' field is required when type='audio'")
                })?;
                let source: AudioSource =
                    serde_json::from_value(source_value).map_err(|error| {
                        de::Error::custom(format!("type='audio' requires an AudioSource: {error}"))
                    })?;
                Ok(Self::Audio { source })
            }
            other => Err(de::Error::custom(format!(
                "unsupported ContentPart type `{other}`; expected 'text', 'image', or 'audio'"
            ))),
        }
    }
}
