use crate::{
    auth::authorize,
    publication::{archive_error, Descriptor},
    types::{collection_id, identifier},
    *,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ctx_history_core::{
    CoreContentPolicyStatus, CoreRecord, SourceKey, MAX_ENCODED_CORE_RECORD_BYTES,
};
use ctx_history_index::{CompiledSearchFilter, EventSearchFilters, LexicalExecution, LexicalMode};
use ctx_history_read_application::search_content_snippet;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub q: String,
    #[serde(default = "search_limit")]
    pub limit: usize,
}
fn search_limit() -> usize {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub status: CollectionStatus,
    pub results: Vec<HostedSearchHit>,
    pub complete: bool,
    pub exhaustive: bool,
}

/// Search presentation only. Exact retained Core content is served by the
/// event/session citations; a shortened snippet is never a shortened CoreRecord.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedSearchHit {
    pub event_id: uuid::Uuid,
    pub session_id: uuid::Uuid,
    pub event_sequence: u64,
    pub occurred_at_unix_ms: Option<i64>,
    pub event_type: String,
    pub role: Option<String>,
    pub snippet: String,
    pub snippet_truncated: bool,
    pub content_status: CoreContentPolicyStatus,
    pub provenance: Provenance,
    pub citation: String,
    pub session_citation: String,
    pub score: Option<f32>,
}

pub const SEARCH_RESPONSE_MAX_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    pub collection: String,
    pub publication: String,
    pub revision: String,
    pub publisher: String,
    pub origin: String,
    pub view: String,
    pub source: SourceKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedEvent {
    pub record: CoreRecord,
    pub provenance: Provenance,
    pub citation: String,
    pub session_citation: String,
    pub score: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationKind {
    Event,
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Citation {
    pub collection: String,
    pub publication: String,
    pub revision: String,
    pub id: String,
    pub kind: CitationKind,
}

impl Citation {
    pub fn encode(&self) -> Result<String> {
        self.validate()?;
        Ok(format!(
            "ctxh1_{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?)
        ))
    }
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() > 4096 {
            return Err(Error::Invalid("citation length"));
        }
        let encoded = value
            .strip_prefix("ctxh1_")
            .ok_or(Error::Invalid("hosted citation required"))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| Error::Invalid("citation encoding"))?;
        let citation: Self = serde_json::from_slice(&bytes)?;
        citation.validate()?;
        Ok(citation)
    }
    fn validate(&self) -> Result<()> {
        collection_id(&self.collection)?;
        collection_id(&self.id)?;
        identifier(&self.publication)?;
        identifier(&self.revision)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRequest {
    #[serde(default = "session_limit")]
    pub limit: usize,
    #[serde(default)]
    pub cursor: Option<String>,
}
fn session_limit() -> usize {
    50
}
impl Default for SessionRequest {
    fn default() -> Self {
        Self {
            limit: 50,
            cursor: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPage {
    pub events: Vec<HostedEvent>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    citation: Citation,
    sequence: u64,
    event: String,
}

struct EventRef {
    event: String,
    sequence: u64,
    offset: u64,
    bytes: usize,
    digest: Vec<u8>,
}

impl HistoryServer {
    pub fn search(
        &self,
        token: &str,
        collection: &str,
        request: SearchRequest,
    ) -> Result<SearchResponse> {
        if request.limit == 0 || request.limit > 100 || request.q.trim().is_empty() {
            return Err(Error::Invalid(
                "nonempty lexical query and limit 1..100 required",
            ));
        }
        let connection = self.lock()?;
        authorize(&connection, token, collection, Access::Read)?;
        let status = self.status_locked(&connection, collection)?;
        if status.stored_sequence == 0 {
            return Ok(SearchResponse {
                status,
                results: vec![],
                complete: true,
                exhaustive: true,
            });
        }
        let index = self.safe_index(&connection, collection)?;
        let filter = CompiledSearchFilter::compile(EventSearchFilters::default())?;
        let query = [request.q.as_str()];
        let batch = index
            .execute_lexical(LexicalExecution::new(
                LexicalMode::Search(&query),
                &filter,
                request.limit,
            ))
            .map_err(|failure| Error::Index(failure.error))?
            .batch;
        let caught_up = status.searchable_sequence == status.stored_sequence;
        let mut response = SearchResponse {
            status,
            results: Vec::new(),
            complete: false,
            exhaustive: false,
        };
        // Count the complete JSON envelope, escaped metadata/citations and array
        // commas. Reserving false flags also covers the shorter true spelling.
        let mut bytes = serde_json::to_vec(&response)?.len();
        let mut bounded = false;
        for candidate in &batch.candidates {
            let record = index
                .core_record_by_id(candidate.event.event_id)?
                .ok_or(Error::Unavailable)?;
            let (publication,revision,descriptor):(String,String,String)=connection.query_row(
                "SELECT publication,revision,descriptor FROM revisions WHERE collection=?1 AND source=?2",
                params![collection,serde_json::to_string(&record.source)?],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            let hit = hosted_search_hit(
                hosted_event(
                    collection,
                    &publication,
                    &revision,
                    record,
                    serde_json::from_str(&descriptor)?,
                    Some(candidate.score),
                )?,
                &query,
            )?;
            let size = serde_json::to_vec(&hit)?.len() + usize::from(!response.results.is_empty());
            if bytes + size > SEARCH_RESPONSE_MAX_BYTES {
                bounded = true;
                break;
            }
            bytes += size;
            response.results.push(hit);
        }
        response.complete = caught_up && batch.complete && !bounded;
        response.exhaustive = caught_up && batch.candidate_set_exhaustive && !bounded;
        Ok(response)
    }

    pub fn read_event(&self, token: &str, collection: &str, value: &str) -> Result<HostedEvent> {
        let connection = self.lock()?;
        authorize(&connection, token, collection, Access::Read)?;
        self.read_gate(&connection, collection)?;
        let citation = checked_citation(collection, value, CitationKind::Event)?;
        let (descriptor, payload) = retained_revision(&connection, &citation)?;
        let reference=connection.query_row("SELECT event,sequence,offset,bytes,digest FROM event_refs WHERE collection=?1 AND publication=?2 AND revision=?3 AND event=?4",
            params![collection,citation.publication,citation.revision,citation.id],event_ref).optional()?.ok_or(Error::NotFound)?;
        self.read_reference(collection, &citation, descriptor, &payload, reference)
    }

    pub fn read_session(
        &self,
        token: &str,
        collection: &str,
        value: &str,
        request: SessionRequest,
    ) -> Result<SessionPage> {
        if request.limit == 0 || request.limit > 100 {
            return Err(Error::Invalid("session page limit must be 1..100"));
        }
        let connection = self.lock()?;
        authorize(&connection, token, collection, Access::Read)?;
        self.read_gate(&connection, collection)?;
        let citation = checked_citation(collection, value, CitationKind::Session)?;
        let cursor: Option<Cursor> = request
            .cursor
            .as_deref()
            .map(|text| {
                if text.len() > 8192 {
                    return Err(Error::Invalid("cursor length"));
                }
                let bytes = URL_SAFE_NO_PAD
                    .decode(text)
                    .map_err(|_| Error::Invalid("cursor encoding"))?;
                Ok(serde_json::from_slice::<Cursor>(&bytes)?)
            })
            .transpose()?;
        if cursor
            .as_ref()
            .is_some_and(|cursor| cursor.citation != citation)
        {
            return Err(Error::Invalid("cursor belongs to another session"));
        }
        let (descriptor, payload) = retained_revision(&connection, &citation)?;
        let mapped_session =
            ctx_history_archive::mapped_session(&descriptor.binding, descriptor.member.session_id)
                .map_err(archive_error)?;
        if mapped_session.as_uuid().to_string() != citation.id {
            return Err(Error::NotFound);
        }
        let mut statement=connection.prepare("SELECT event,sequence,offset,bytes,digest FROM event_refs WHERE collection=?1 AND publication=?2 AND revision=?3 AND session=?4 AND (?5=0 OR (sequence,event)>(?6,?7)) ORDER BY sequence,event LIMIT ?8")?;
        let mut rows = statement.query(params![
            collection,
            citation.publication,
            citation.revision,
            citation.id,
            cursor.is_some(),
            cursor.as_ref().map_or(0, |c| c.sequence),
            cursor.as_ref().map_or("", |c| c.event.as_str()),
            request.limit as u64 + 1
        ])?;
        let mut events = Vec::new();
        let mut bytes = 0;
        let mut last = None;
        let mut has_more = false;
        while let Some(row) = rows.next()? {
            let reference = event_ref(row)?;
            if events.len() == request.limit
                || bytes + reference.bytes > MAX_ENCODED_CORE_RECORD_BYTES
            {
                has_more = true;
                break;
            }
            bytes += reference.bytes;
            last = Some(Cursor {
                citation: citation.clone(),
                sequence: reference.sequence,
                event: reference.event.clone(),
            });
            events.push(self.read_reference(
                collection,
                &citation,
                descriptor.clone(),
                &payload,
                reference,
            )?);
        }
        let next_cursor = if has_more {
            let cursor = last.ok_or(Error::Capacity)?;
            Some(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&cursor)?))
        } else {
            None
        };
        Ok(SessionPage {
            events,
            next_cursor,
        })
    }

    fn read_reference(
        &self,
        collection: &str,
        citation: &Citation,
        descriptor: Descriptor,
        payload: &UploadSpec,
        reference: EventRef,
    ) -> Result<HostedEvent> {
        if reference.bytes > MAX_ENCODED_CORE_RECORD_BYTES
            || reference
                .offset
                .checked_add(reference.bytes as u64)
                .is_none_or(|end| end > payload.bytes)
        {
            return Err(Error::Unavailable);
        }
        crate::storage::valid_spec(payload)?;
        let mut file = File::open(
            self.collection_root(collection)
                .join("payloads")
                .join(&payload.sha256),
        )?;
        file.seek(SeekFrom::Start(reference.offset))?;
        let mut bytes = vec![0; reference.bytes];
        file.read_exact(&mut bytes)?;
        if Sha256::digest(&bytes)[..] != reference.digest {
            return Err(Error::Unavailable);
        }
        let record = CoreRecord::decode_stored(&bytes)?;
        let mapped =
            ctx_history_archive::map_record(&descriptor.binding, &descriptor.member, record)
                .map_err(archive_error)?;
        if mapped.event_id.as_uuid().to_string() != reference.event {
            return Err(Error::Unavailable);
        }
        hosted_event(
            collection,
            &citation.publication,
            &citation.revision,
            mapped,
            descriptor,
            None,
        )
    }
}

fn checked_citation(collection: &str, value: &str, kind: CitationKind) -> Result<Citation> {
    let citation = Citation::parse(value)?;
    if citation.collection != collection || citation.kind != kind {
        return Err(Error::NotFound);
    }
    Ok(citation)
}

fn event_ref(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRef> {
    Ok(EventRef {
        event: row.get(0)?,
        sequence: row.get(1)?,
        offset: row.get(2)?,
        bytes: row.get(3)?,
        digest: row.get(4)?,
    })
}

fn retained_revision(
    connection: &Connection,
    citation: &Citation,
) -> Result<(Descriptor, UploadSpec)> {
    let (descriptor,payload):(String,String)=connection.query_row(
        "SELECT r.descriptor,r.payload FROM revisions r JOIN publications p ON p.collection=r.collection AND p.publication=r.publication WHERE r.collection=?1 AND r.publication=?2 AND r.revision=?3 AND p.withdrawn=0",
        params![citation.collection,citation.publication,citation.revision],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or(Error::NotFound)?;
    Ok((
        serde_json::from_str(&descriptor)?,
        serde_json::from_str(&payload)?,
    ))
}

fn hosted_search_hit(event: HostedEvent, query: &[&str]) -> Result<HostedSearchHit> {
    let HostedEvent {
        record,
        provenance,
        citation,
        session_citation,
        score,
    } = event;
    let content_status = record.content.policy_status.clone();
    let (snippet, snippet_truncated) = search_content_snippet(record.content, query)
        .map_err(|_| Error::Unavailable)?
        .unwrap_or_default();
    Ok(HostedSearchHit {
        event_id: record.event_id.as_uuid(),
        session_id: record.session_id.as_uuid(),
        event_sequence: record.event_sequence,
        occurred_at_unix_ms: record.occurred_at_unix_ms,
        event_type: record.event_type,
        role: record.role,
        snippet,
        snippet_truncated,
        content_status,
        provenance,
        citation,
        session_citation,
        score,
    })
}

fn hosted_event(
    collection: &str,
    publication: &str,
    revision: &str,
    record: CoreRecord,
    descriptor: Descriptor,
    score: Option<f32>,
) -> Result<HostedEvent> {
    let citation = Citation {
        collection: collection.into(),
        publication: publication.into(),
        revision: revision.into(),
        id: record.event_id.as_uuid().to_string(),
        kind: CitationKind::Event,
    };
    let session = Citation {
        id: record.session_id.as_uuid().to_string(),
        kind: CitationKind::Session,
        ..citation.clone()
    };
    Ok(HostedEvent {
        citation: citation.encode()?,
        session_citation: session.encode()?,
        record,
        score,
        provenance: Provenance {
            collection: collection.into(),
            publication: publication.into(),
            revision: revision.into(),
            publisher: descriptor.publisher,
            origin: descriptor.binding.identity.origin,
            view: descriptor.binding.identity.view,
            source: descriptor.member.source,
        },
    })
}
