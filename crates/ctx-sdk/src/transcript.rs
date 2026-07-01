use ctx_history_core::{CaptureSource, Event, EventRole, EventType, Session};
use ctx_history_store::Store;
use uuid::Uuid;

use crate::{client::CtxClient, error::Result};

/// Transcript compaction mode, matching the CLI modes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TranscriptMode {
    Full,
    #[default]
    Lite,
    Log,
}

impl TranscriptMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Lite => "lite",
            Self::Log => "log",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShowSessionOptions {
    pub mode: TranscriptMode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionTranscript {
    pub session: Session,
    pub events: Vec<Event>,
    pub mode: TranscriptMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventWindowOptions {
    pub before: usize,
    pub after: usize,
    pub window: Option<usize>,
}

impl EventWindowOptions {
    pub fn window(window: usize) -> Self {
        Self {
            before: 0,
            after: 0,
            window: Some(window),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventWindow {
    pub event: Event,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionLocation {
    pub session: Session,
    pub source: Option<CaptureSource>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventLocation {
    pub event: Event,
    pub session: Option<Session>,
    pub source: Option<CaptureSource>,
}

impl CtxClient {
    /// Load one session transcript using a read-only connection.
    pub fn show_session(
        &self,
        session_id: Uuid,
        options: ShowSessionOptions,
    ) -> Result<SessionTranscript> {
        let store = self.open_store_read_only()?;
        let session = store.get_session(session_id)?;
        let events = store.events_for_session(session.id)?;
        Ok(SessionTranscript {
            session,
            events: selected_transcript_events(&events, options.mode),
            mode: options.mode,
        })
    }

    /// Load an event and nearby events using a read-only connection.
    pub fn show_event(&self, event_id: Uuid, options: EventWindowOptions) -> Result<EventWindow> {
        let store = self.open_store_read_only()?;
        let event = store.get_event(event_id)?;
        let events = event_window(
            &store,
            &event,
            options.before,
            options.after,
            options.window,
        )?;
        Ok(EventWindow { event, events })
    }

    pub fn locate_session(&self, session_id: Uuid) -> Result<SessionLocation> {
        let store = self.open_store_read_only()?;
        let session = store.get_session(session_id)?;
        let source = session
            .capture_source_id
            .and_then(|source_id| store.get_capture_source(source_id).ok());
        Ok(SessionLocation { session, source })
    }

    pub fn locate_event(&self, event_id: Uuid) -> Result<EventLocation> {
        let store = self.open_store_read_only()?;
        let event = store.get_event(event_id)?;
        let session = event
            .session_id
            .and_then(|session_id| store.get_session(session_id).ok());
        let source = event
            .capture_source_id
            .and_then(|source_id| store.get_capture_source(source_id).ok());
        Ok(EventLocation {
            event,
            session,
            source,
        })
    }
}

fn event_window(
    store: &Store,
    event: &Event,
    before: usize,
    after: usize,
    window: Option<usize>,
) -> Result<Vec<Event>> {
    let Some(session_id) = event.session_id else {
        return Ok(vec![event.clone()]);
    };
    let events = store.events_for_session(session_id)?;
    let Some(index) = events.iter().position(|candidate| candidate.id == event.id) else {
        return Ok(vec![event.clone()]);
    };
    let (before, after) = window
        .map(|window| (window, window))
        .unwrap_or((before, after));
    let start = index.saturating_sub(before);
    let end = (index + after + 1).min(events.len());
    Ok(events[start..end].to_vec())
}

fn selected_transcript_events(events: &[Event], mode: TranscriptMode) -> Vec<Event> {
    match mode {
        TranscriptMode::Log => events.to_vec(),
        TranscriptMode::Full => events
            .iter()
            .filter(|event| is_message(event))
            .cloned()
            .collect(),
        TranscriptMode::Lite => lite_transcript_events(events),
    }
}

fn lite_transcript_events(events: &[Event]) -> Vec<Event> {
    let mut selected = Vec::new();
    let mut pending_assistant: Option<&Event> = None;
    for event in events {
        if is_user_message(event) {
            if let Some(assistant) = pending_assistant.take() {
                selected.push(assistant.clone());
            }
            selected.push(event.clone());
        } else if is_assistant_message(event) {
            pending_assistant = Some(event);
        }
    }
    if let Some(assistant) = pending_assistant {
        selected.push(assistant.clone());
    }
    selected
}

fn is_message(event: &Event) -> bool {
    event.event_type == EventType::Message
        && matches!(
            event.role,
            Some(EventRole::User | EventRole::Assistant | EventRole::System)
        )
}

fn is_user_message(event: &Event) -> bool {
    event.event_type == EventType::Message && event.role == Some(EventRole::User)
}

fn is_assistant_message(event: &Event) -> bool {
    event.event_type == EventType::Message && event.role == Some(EventRole::Assistant)
}
