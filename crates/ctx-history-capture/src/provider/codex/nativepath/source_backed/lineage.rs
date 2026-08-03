use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    sync::Mutex,
};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::*;
use crate::common::io::{read_provider_jsonl_line_or_skip_oversized, ProviderJsonlLineRead};
use crate::provider::codex::nativepath::checkpoint::MAX_CODEX_TOOL_CALL_ID_BYTES;
use crate::provider::codex::nativepath::reader::{
    open_codex_source_capability, revalidate_codex_source_observation,
};

const LINEAGE_DEPENDENCY_DOMAIN: &[u8] = b"ctx/codex-lineage-dependency/v1\0";
const MAX_LINEAGE_FACTS_PER_TASK: usize = 262_144;
const MAX_LINEAGE_FACT_BYTES_PER_TASK: usize = 64 * 1024 * 1024;

#[cfg(test)]
type AfterLineagePrefixHookV0 = (std::path::PathBuf, Box<dyn FnOnce(&std::path::Path) + Send>);

#[cfg(test)]
static AFTER_LINEAGE_PREFIX_HOOK_V0: Mutex<Option<AfterLineagePrefixHookV0>> = Mutex::new(None);

#[cfg(test)]
pub(super) fn install_after_lineage_prefix_hook_v0(
    expected_path: std::path::PathBuf,
    hook: Box<dyn FnOnce(&std::path::Path) + Send>,
) {
    let mut slot = AFTER_LINEAGE_PREFIX_HOOK_V0
        .lock()
        .expect("lineage test hook lock");
    assert!(slot.is_none(), "lineage test hook is already installed");
    *slot = Some((expected_path, hook));
}

#[cfg(test)]
fn run_after_lineage_prefix_hook_v0(path: &std::path::Path) {
    let hook = AFTER_LINEAGE_PREFIX_HOOK_V0
        .lock()
        .ok()
        .and_then(|mut slot| {
            if slot.as_ref().is_some_and(|(expected, _)| expected == path) {
                slot.take().map(|(_, hook)| hook)
            } else {
                None
            }
        });
    if let Some(hook) = hook {
        hook(path);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexOutcomeOriginV0 {
    UniqueToSession,
    CopiedFromAncestor,
    Unproven,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AncestorCallPresenceV0 {
    Present,
    Absent,
    Unproven,
}

#[derive(Debug, Default)]
struct AncestorLineageFactsV0 {
    calls: HashSet<String>,
    results: HashSet<String>,
    ambiguous: HashSet<String>,
    has_unattributed_ambiguity: bool,
    retained_bytes: usize,
}

impl AncestorLineageFactsV0 {
    fn insert(
        &mut self,
        target: LineageFactTargetV0,
        call_id: &str,
        remaining_facts: usize,
        remaining_bytes: usize,
    ) -> CodexSourceBackedResultV0<()> {
        if call_id.is_empty() {
            return Ok(());
        }
        let already_present = match target {
            LineageFactTargetV0::Call => self.calls.contains(call_id),
            LineageFactTargetV0::Result => self.results.contains(call_id),
            LineageFactTargetV0::Ambiguous => self.ambiguous.contains(call_id),
        };
        if already_present {
            return Ok(());
        }
        let observed_count = self
            .calls
            .len()
            .checked_add(self.results.len())
            .and_then(|count| count.checked_add(self.ambiguous.len()))
            .and_then(|count| count.checked_add(1))
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?;
        let observed_bytes = self
            .retained_bytes
            .checked_add(call_id.len())
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?;
        if observed_count > remaining_facts || observed_bytes > remaining_bytes {
            return Err(CodexSourceBackedErrorV0::LineageWorkingSetExhausted);
        }
        match target {
            LineageFactTargetV0::Call => self.calls.insert(call_id.to_owned()),
            LineageFactTargetV0::Result => self.results.insert(call_id.to_owned()),
            LineageFactTargetV0::Ambiguous => self.ambiguous.insert(call_id.to_owned()),
        };
        self.retained_bytes = observed_bytes;
        Ok(())
    }

    fn fact_count(&self) -> CodexSourceBackedResultV0<usize> {
        self.calls
            .len()
            .checked_add(self.results.len())
            .and_then(|count| count.checked_add(self.ambiguous.len()))
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetExhausted)
    }

    fn presence(&self, origin_call_id: &str, result_call_id: &str) -> AncestorCallPresenceV0 {
        if self.calls.contains(origin_call_id) && self.results.contains(result_call_id) {
            AncestorCallPresenceV0::Present
        } else if self.has_unattributed_ambiguity
            || self.ambiguous.contains(origin_call_id)
            || self.ambiguous.contains(result_call_id)
            || self.calls.contains(origin_call_id)
            || self.results.contains(result_call_id)
        {
            AncestorCallPresenceV0::Unproven
        } else {
            AncestorCallPresenceV0::Absent
        }
    }
}

enum LineageFactTargetV0 {
    Call,
    Result,
    Ambiguous,
}

#[derive(Debug)]
pub(super) struct CodexOutcomeLineageAuthorityV0 {
    sources: HashMap<String, CodexCatalogSource>,
    dependency_digests: HashMap<String, [u8; 32]>,
    facts: Mutex<HashMap<String, AncestorLineageFactsV0>>,
}

impl CodexOutcomeLineageAuthorityV0 {
    pub(super) fn from_sources(sources: &[(CodexCatalogSource, SourceKey, String)]) -> Self {
        let sources = sources
            .iter()
            .map(|(source, _, native_session_id)| (native_session_id.clone(), source.clone()))
            .collect::<HashMap<_, _>>();
        let dependency_digests = sources
            .keys()
            .map(|native_session_id| {
                (
                    native_session_id.clone(),
                    lineage_dependency_digest(&sources, native_session_id),
                )
            })
            .collect();
        Self {
            sources,
            dependency_digests,
            facts: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub(super) fn unscoped() -> Self {
        Self {
            sources: HashMap::new(),
            dependency_digests: HashMap::new(),
            facts: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn dependency_digest(&self, native_session_id: &str) -> [u8; 32] {
        self.dependency_digests
            .get(native_session_id)
            .copied()
            .unwrap_or_else(|| lineage_dependency_digest(&self.sources, native_session_id))
    }

    pub(super) fn classify(
        &self,
        native_session_id: &str,
        origin_call_id: &str,
        result_call_id: &str,
    ) -> CodexSourceBackedResultV0<CodexOutcomeOriginV0> {
        let Some(current) = self.sources.get(native_session_id) else {
            return Ok(CodexOutcomeOriginV0::Unproven);
        };
        let mut parent = current.catalog_parent_native_session_id.as_deref();
        let mut visited = HashSet::new();

        while let Some(parent_id) = parent {
            if !visited.insert(parent_id.to_owned()) {
                return Ok(CodexOutcomeOriginV0::Unproven);
            }
            let Some(parent_source) = self.sources.get(parent_id) else {
                return Ok(CodexOutcomeOriginV0::Unproven);
            };
            match self.ancestor_call_presence(
                parent_id,
                parent_source,
                origin_call_id,
                result_call_id,
            )? {
                AncestorCallPresenceV0::Present => {
                    return Ok(CodexOutcomeOriginV0::CopiedFromAncestor)
                }
                AncestorCallPresenceV0::Absent => {
                    parent = parent_source.catalog_parent_native_session_id.as_deref();
                }
                AncestorCallPresenceV0::Unproven => return Ok(CodexOutcomeOriginV0::Unproven),
            }
        }
        Ok(CodexOutcomeOriginV0::UniqueToSession)
    }

    fn ancestor_call_presence(
        &self,
        native_session_id: &str,
        source: &CodexCatalogSource,
        origin_call_id: &str,
        result_call_id: &str,
    ) -> CodexSourceBackedResultV0<AncestorCallPresenceV0> {
        if origin_call_id.is_empty() || result_call_id.is_empty() {
            return Ok(AncestorCallPresenceV0::Unproven);
        }
        let mut facts = self
            .facts
            .lock()
            .map_err(|_| CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)?;
        if !facts.contains_key(native_session_id) {
            let (retained_facts, retained_bytes) = facts.values().try_fold(
                (0_usize, 0_usize),
                |(retained_facts, retained_bytes), source_facts| {
                    Ok::<_, CodexSourceBackedErrorV0>((
                        retained_facts
                            .checked_add(source_facts.fact_count()?)
                            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?,
                        retained_bytes
                            .checked_add(source_facts.retained_bytes)
                            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?,
                    ))
                },
            )?;
            let remaining_facts = MAX_LINEAGE_FACTS_PER_TASK
                .checked_sub(retained_facts)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?;
            let remaining_bytes = MAX_LINEAGE_FACT_BYTES_PER_TASK
                .checked_sub(retained_bytes)
                .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetExhausted)?;
            facts.insert(
                native_session_id.to_owned(),
                scan_ancestor_lineage_facts(source, remaining_facts, remaining_bytes)?,
            );
        }
        facts
            .get(native_session_id)
            .map(|facts| facts.presence(origin_call_id, result_call_id))
            .ok_or(CodexSourceBackedErrorV0::LineageWorkingSetUnavailable)
    }
}

fn lineage_dependency_digest(
    sources: &HashMap<String, CodexCatalogSource>,
    native_session_id: &str,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(LINEAGE_DEPENDENCY_DOMAIN);
    let mut parent = sources
        .get(native_session_id)
        .and_then(|source| source.catalog_parent_native_session_id.as_deref());
    let mut visited = HashSet::new();
    while let Some(parent_id) = parent {
        hash_text(&mut hasher, parent_id);
        if !visited.insert(parent_id.to_owned()) {
            hasher.update(b"cycle\0");
            break;
        }
        let Some(source) = sources.get(parent_id) else {
            hasher.update(b"missing\0");
            break;
        };
        match serde_json::to_vec(&source.catalog_observation) {
            Ok(observation) => {
                hasher.update((observation.len() as u64).to_le_bytes());
                hasher.update(observation);
            }
            Err(_) => hasher.update(b"invalid-observation\0"),
        }
        parent = source.catalog_parent_native_session_id.as_deref();
    }
    hasher.finalize().into()
}

fn hash_text(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn scan_ancestor_lineage_facts(
    source: &CodexCatalogSource,
    remaining_facts: usize,
    remaining_bytes: usize,
) -> CodexSourceBackedResultV0<AncestorLineageFactsV0> {
    let opened = open_codex_source_capability(source)?;
    let current = opened_codex_file_observation(&source.source_path, opened.file())?;
    opened.revalidate_same_object()?;
    if !source
        .catalog_observation
        .admits_append_only_growth(&current)
    {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }

    let mut file = opened.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let bounded = file.take(source.catalog_observation.len);
    let mut reader = HashingBufReader::new(bounded);
    let mut line = Vec::new();
    let mut facts = AncestorLineageFactsV0::default();
    loop {
        match read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)? {
            ProviderJsonlLineRead::Eof => break,
            ProviderJsonlLineRead::Oversized { .. } => {
                // Oversized relationship evidence is not safely attributable
                // to a particular call ID and therefore cannot prove lineage.
                facts.has_unattributed_ambiguity = true;
            }
            ProviderJsonlLineRead::Line { .. } => {
                if !contains_bytes(&line, br#""call_id""#) {
                    continue;
                }
                if !line.ends_with(b"\n") {
                    let call_ids = lexical_call_ids(&line);
                    facts.has_unattributed_ambiguity |= call_ids.is_empty();
                    for call_id in call_ids {
                        facts.insert(
                            LineageFactTargetV0::Ambiguous,
                            &call_id,
                            remaining_facts,
                            remaining_bytes,
                        )?;
                    }
                    continue;
                }
                match serde_json::from_slice::<Value>(&line) {
                    Ok(record) => {
                        if let Some(call_id) = structured_tool_call_id(&record) {
                            facts.insert(
                                LineageFactTargetV0::Call,
                                call_id,
                                remaining_facts,
                                remaining_bytes,
                            )?;
                        }
                        if let Some(call_id) = structured_tool_result_id(&record) {
                            facts.insert(
                                LineageFactTargetV0::Result,
                                call_id,
                                remaining_facts,
                                remaining_bytes,
                            )?;
                        }
                    }
                    Err(_) => {
                        let call_ids = lexical_call_ids(&line);
                        facts.has_unattributed_ambiguity |= call_ids.is_empty();
                        for call_id in call_ids {
                            facts.insert(
                                LineageFactTargetV0::Ambiguous,
                                &call_id,
                                remaining_facts,
                                remaining_bytes,
                            )?;
                        }
                    }
                }
            }
        }
    }
    let digest = reader.finalize();
    #[cfg(test)]
    run_after_lineage_prefix_hook_v0(&source.source_path);
    revalidate_codex_source_observation(
        source,
        &source.catalog_observation,
        source.catalog_observation.len,
        digest,
    )?;
    Ok(facts)
}

fn structured_tool_call_id(record: &Value) -> Option<&str> {
    (record.get("type").and_then(Value::as_str) == Some("response_item"))
        .then(|| record.get("payload"))
        .flatten()
        .filter(|payload| {
            matches!(
                payload.get("type").and_then(Value::as_str),
                Some("function_call" | "custom_tool_call" | "web_search_call" | "tool_search_call")
            )
        })
        .and_then(|payload| payload.get("call_id"))
        .and_then(Value::as_str)
}

fn structured_tool_result_id(record: &Value) -> Option<&str> {
    let record_type = record.get("type").and_then(Value::as_str)?;
    if !matches!(record_type, "response_item" | "event_msg") {
        return None;
    }
    let payload = record.get("payload")?;
    let item_type = payload.get("type").and_then(Value::as_str)?;
    (item_type.ends_with("_output")
        || matches!(
            item_type,
            "patch_apply_end"
                | "web_search_end"
                | "exec_command_end"
                | "command_complete"
                | "tool_complete"
        ))
    .then(|| payload.get("call_id"))
    .flatten()
    .and_then(Value::as_str)
}

fn lexical_call_ids(record: &[u8]) -> Vec<String> {
    let Ok(text) = std::str::from_utf8(record) else {
        return Vec::new();
    };
    let mut remaining = text;
    let mut values = Vec::new();
    while let Some(index) = remaining.find("\"call_id\"") {
        remaining = &remaining[index + "\"call_id\"".len()..];
        let Some(colon) = remaining.find(':') else {
            break;
        };
        remaining = remaining[colon + 1..].trim_start();
        let Some(rest) = remaining.strip_prefix('"') else {
            continue;
        };
        let Some(end) = rest.find('"') else {
            break;
        };
        if end != 0 && end <= MAX_CODEX_TOOL_CALL_ID_BYTES {
            values.push(rest[..end].to_owned());
        }
        remaining = &rest[end + 1..];
    }
    values
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|candidate| candidate == needle)
}

struct HashingBufReader<R> {
    inner: BufReader<R>,
    hasher: Sha256,
}

impl<R: Read> HashingBufReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner: BufReader::new(inner),
            hasher: Sha256::new(),
        }
    }

    fn finalize(self) -> [u8; 32] {
        self.hasher.finalize().into()
    }
}

impl<R: Read> Read for HashingBufReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.hasher.update(&buffer[..read]);
        Ok(read)
    }
}

impl<R: Read> BufRead for HashingBufReader<R> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        let buffer = self.inner.buffer();
        let consumed = amount.min(buffer.len());
        self.hasher.update(&buffer[..consumed]);
        self.inner.consume(consumed);
    }
}
