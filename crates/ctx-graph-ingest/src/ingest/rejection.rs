use serde::Serialize;

/// A known syntax or encoding error in local input, not an extraction failure.
/// Only directory scans may replace this file's facts with a diagnostic.
#[derive(Debug, Serialize)]
pub struct InputRejected {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    pub message: String,
}

impl std::fmt::Display for InputRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.file)?;
        if let Some(document) = self.document {
            write!(f, " (document {document})")?;
        }
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
            if let Some(column) = self.column {
                write!(f, ":{column}")?;
            }
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for InputRejected {}

pub(super) fn utf8<'a>(file: &str, bytes: &'a [u8]) -> Result<&'a str, InputRejected> {
    std::str::from_utf8(bytes).map_err(|error| InputRejected {
        file: file.into(),
        document: None,
        line: None,
        column: None,
        message: format!("document is not UTF-8 at byte {}", error.valid_up_to()),
    })
}

pub(super) fn yaml_error(
    file: &str,
    text: &str,
    document: Option<usize>,
    line_offset: usize,
    error: serde_yaml_ng::Error,
) -> anyhow::Error {
    use serde::Deserialize;
    // Value decoding also has recursion/alias budgets. The native syntax-only
    // visitor skips value construction and alias expansion; only its failures
    // are known input syntax errors. Other decoder failures remain fatal.
    let syntax_error = serde_yaml_ng::Deserializer::from_str(text)
        .nth(document.unwrap_or(1) - 1)
        .is_some_and(|deserializer| serde::de::IgnoredAny::deserialize(deserializer).is_err());
    let format = if document.is_some() {
        "YAML"
    } else {
        "Markdown frontmatter"
    };
    if !syntax_error {
        return anyhow::Error::new(error).context(format!("{file}: {format} decoding failed"));
    }
    let location = error.location();
    InputRejected {
        file: file.into(),
        document,
        line: location
            .as_ref()
            .map(|location| location.line() + line_offset),
        column: location.as_ref().map(|location| location.column()),
        message: format!("invalid {format}: {error}"),
    }
    .into()
}

pub(super) fn pointer_json_error(
    file: &str,
    text: &str,
    error: serde_json::Error,
) -> anyhow::Error {
    // serde_json classifies its recursion limit as Syntax too. IgnoredAny skips
    // nested values iteratively, so the category alone is insufficient here.
    if !(error.is_syntax() || error.is_eof())
        || serde_json::from_str::<serde::de::IgnoredAny>(text).is_ok()
    {
        return anyhow::Error::new(error)
            .context(format!("{file}: Google pointer JSON decoding failed"));
    }
    InputRejected {
        file: file.into(),
        document: None,
        line: Some(error.line()),
        column: Some(error.column()),
        message: format!("invalid Google pointer JSON: {error}"),
    }
    .into()
}

/// Adapter output is not authoritative native source syntax. Do not retain the
/// rejection marker in the error chain: a scan must preserve the previous graph.
pub(super) fn adapter_output(error: anyhow::Error) -> anyhow::Error {
    match error.downcast::<InputRejected>() {
        Ok(rejection) => {
            anyhow::anyhow!("document adapter output could not be parsed: {rejection}")
        }
        Err(error) => error,
    }
}
