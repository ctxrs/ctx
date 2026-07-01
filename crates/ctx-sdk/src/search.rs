use chrono::{DateTime, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_search::{PacketOptions, SearchPacket, SearchResultMode};
use uuid::Uuid;

use crate::{client::CtxClient, error::Result};

/// Search options for the existing local index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOptions {
    pub packet: PacketOptions,
    pub terms: Vec<String>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            packet: PacketOptions::default(),
            terms: Vec::new(),
        }
    }
}

impl SearchOptions {
    pub fn limit(mut self, limit: usize) -> Self {
        self.packet.limit = limit;
        self
    }

    pub fn provider(mut self, provider: CaptureProvider) -> Self {
        self.packet.filters.provider = Some(provider);
        self
    }

    pub fn workspace(mut self, workspace: impl Into<String>) -> Self {
        self.packet.filters.repo = Some(workspace.into());
        self
    }

    pub fn since(mut self, since: DateTime<Utc>) -> Self {
        self.packet.filters.since = Some(since);
        self
    }

    pub fn file(mut self, file: impl Into<String>) -> Self {
        self.packet.filters.file = Some(file.into());
        self
    }

    pub fn session(mut self, session: Uuid) -> Self {
        self.packet.filters.session = Some(session);
        self.packet.result_mode = SearchResultMode::Events;
        self
    }

    pub fn events(mut self) -> Self {
        self.packet.result_mode = SearchResultMode::Events;
        self
    }

    pub fn term(mut self, term: impl Into<String>) -> Self {
        self.terms.push(term.into());
        self
    }
}

impl CtxClient {
    /// Search the existing local index using a read-only connection.
    pub fn search(&self, query: impl AsRef<str>, options: SearchOptions) -> Result<SearchPacket> {
        let store = self.open_store_read_only()?;
        let query = query.as_ref();
        if options.terms.iter().any(|term| !term.trim().is_empty()) {
            Ok(ctx_history_search::search_packet_terms(
                &store,
                query,
                &options.terms,
                &options.packet,
            )?)
        } else {
            Ok(ctx_history_search::search_packet(
                &store,
                query,
                &options.packet,
            )?)
        }
    }
}
