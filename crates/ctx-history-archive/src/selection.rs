use rusqlite::Connection;

use crate::{hex, Result, Selection, SourceKey, StableEntityId};

/// A resolved empty table means no sessions, unlike the explicit selector's
/// empty wildcard sets. The plan owns the table through publication.
pub(crate) struct SessionSelection<'a> {
    pub explicit: &'a Selection,
    pub resolved: Option<&'a Connection>,
    pub filtered: bool,
}

impl<'a> SessionSelection<'a> {
    pub fn explicit(selection: &'a Selection) -> Self {
        Self {
            explicit: selection,
            resolved: None,
            filtered: false,
        }
    }

    pub fn includes_source(&self, source: &SourceKey) -> Result<bool> {
        if !self.explicit.includes_source(source) {
            return Ok(false);
        }
        match self.resolved {
            None => Ok(true),
            Some(db) => Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM selected_sessions WHERE source=?1)",
                [hex(&source.identity().digest())],
                |row| row.get(0),
            )?),
        }
    }

    pub fn includes_session(&self, session: StableEntityId) -> Result<bool> {
        if !self.explicit.includes_session(session) {
            return Ok(false);
        }
        match self.resolved {
            None => Ok(true),
            Some(db) => Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM selected_sessions WHERE id=?1)",
                [hex(&session.digest())],
                |row| row.get(0),
            )?),
        }
    }

    pub fn is_subset(&self) -> bool {
        self.filtered || !self.explicit.sources.is_empty() || !self.explicit.sessions.is_empty()
    }
}
