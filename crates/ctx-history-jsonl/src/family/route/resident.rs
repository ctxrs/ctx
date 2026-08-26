use super::*;

#[derive(Clone)]
pub(super) struct AuthenticatedSourceObservation {
    pub(super) certificate: CertifiedSource,
    pub(super) observation: JsonlFileObservation,
}

pub(super) struct FamilyResident<E: JsonlFamilyError> {
    pub(super) ownership_initialized: bool,
    pub(super) owned_sources: HashMap<[u8; 32], SourceKey>,
    pub(super) quarantined_sources: HashMap<[u8; 32], SourceKey>,
    pub(super) terminal_sources: HashMap<[u8; 32], TerminalSourceEvidence<E>>,
    pub(super) absent_sources: Vec<JsonlFamilyAbsentMember<E>>,
    pub(super) opening_membership: Option<JsonlFamilyMembershipObservation<E>>,
    pub(super) certified_inventory: Option<CertifiedSourceInventory>,
    pub(super) opening_inventory: Option<JsonlFamilyInventory<E>>,
    /// Process-local advancement of an immutable certificate's authenticated
    /// live metadata. This is never serialized or used across certificates.
    pub(super) authenticated_source_observations: HashMap<[u8; 32], AuthenticatedSourceObservation>,
}

impl<E: JsonlFamilyError> FamilyResident<E> {
    pub(super) fn replace_terminal_sources(
        &mut self,
        terminal_sources: HashMap<[u8; 32], TerminalSourceEvidence<E>>,
    ) {
        self.authenticated_source_observations
            .retain(|digest, authenticated| {
                terminal_sources
                    .get(digest)
                    .is_some_and(|evidence| evidence.certificate == authenticated.certificate)
            });
        self.terminal_sources = terminal_sources;
    }
}

impl<E: JsonlFamilyError> Default for FamilyResident<E> {
    fn default() -> Self {
        Self {
            ownership_initialized: false,
            owned_sources: HashMap::new(),
            quarantined_sources: HashMap::new(),
            terminal_sources: HashMap::new(),
            absent_sources: Vec::new(),
            opening_membership: None,
            certified_inventory: None,
            opening_inventory: None,
            authenticated_source_observations: HashMap::new(),
        }
    }
}
