//! Manager inventory for reviewing retained history without knowing a search term.
use crate::{auth::authorize, publication::Descriptor, types::identifier, *};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationListRequest {
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default = "page_limit")]
    pub limit: usize,
}

fn page_limit() -> usize {
    50
}

impl Default for PublicationListRequest {
    fn default() -> Self {
        Self {
            after: None,
            limit: page_limit(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationEntry {
    #[serde(flatten)]
    pub state: PublicationState,
    /// This retained revision can differ from the current publication revision.
    pub retained_revision: String,
    /// Exact citations for this retained revision. Withdrawn entries have none.
    pub session_citations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationPage {
    pub publications: Vec<PublicationEntry>,
    pub next_cursor: Option<String>,
}

impl HistoryServer {
    /// Keyset pagination by publication/revision, scoped to one managed collection.
    /// No search generation or matching query is needed to review retained data.
    pub fn list_publications(
        &self,
        token: &str,
        collection: &str,
        request: PublicationListRequest,
    ) -> Result<PublicationPage> {
        if request.limit == 0 || request.limit > 100 {
            return Err(Error::Invalid("publication page limit must be 1..100"));
        }
        let after: Option<(String, String)> = request
            .after
            .as_deref()
            .map(|cursor| {
                if cursor.len() > 1024 {
                    return Err(Error::Invalid("publication cursor length"));
                }
                let bytes = URL_SAFE_NO_PAD
                    .decode(cursor)
                    .map_err(|_| Error::Invalid("publication cursor encoding"))?;
                let pair: (String, String) = serde_json::from_slice(&bytes)?;
                identifier(&pair.0)?;
                identifier(&pair.1)?;
                Ok(pair)
            })
            .transpose()?;
        let connection = self.lock()?;
        authorize(&connection, token, collection, Access::Manage)?;
        let mut statement = connection.prepare(
            "SELECT p.publication,p.owner,p.revision,p.sequence,p.epoch,p.policy,p.withdrawn,r.descriptor,r.revision
             FROM publications p JOIN revisions r
             ON r.collection=p.collection AND r.publication=p.publication
             WHERE p.collection=?1 AND (?2 IS NULL OR (p.publication,r.revision)>(?2,?3))
             ORDER BY p.publication,r.revision LIMIT ?4",
        )?;
        let mut rows = statement.query(params![
            collection,
            after.as_ref().map(|pair| &pair.0),
            after.as_ref().map(|pair| &pair.1),
            request.limit as u64 + 1,
        ])?;
        let mut publications: Vec<PublicationEntry> = Vec::new();
        let mut next_cursor = None;
        while let Some(row) = rows.next()? {
            if publications.len() == request.limit {
                next_cursor = publications
                    .last()
                    .map(|entry| {
                        serde_json::to_vec(&(&entry.state.publication, &entry.retained_revision))
                    })
                    .transpose()?
                    .map(|bytes| URL_SAFE_NO_PAD.encode(bytes));
                break;
            }
            let state = PublicationState {
                publication: row.get(0)?,
                owner: row.get(1)?,
                revision: row.get(2)?,
                sequence: row.get(3)?,
                writer_epoch: row.get(4)?,
                policy_revision: row.get(5)?,
                withdrawn: row.get(6)?,
            };
            let retained_revision: String = row.get(8)?;
            let mut session_citations = Vec::new();
            if !state.withdrawn {
                let descriptor: String = row.get(7)?;
                let descriptor: Descriptor = serde_json::from_str(&descriptor)?;
                let session = ctx_history_archive::mapped_session(
                    &descriptor.binding,
                    descriptor.member.session_id,
                )
                .map_err(publication::archive_error)?;
                session_citations.push(
                    Citation {
                        collection: collection.into(),
                        publication: state.publication.clone(),
                        revision: retained_revision.clone(),
                        id: session.as_uuid().to_string(),
                        kind: CitationKind::Session,
                    }
                    .encode()?,
                );
            }
            publications.push(PublicationEntry {
                state,
                retained_revision,
                session_citations,
            });
        }
        Ok(PublicationPage {
            publications,
            next_cursor,
        })
    }
}
