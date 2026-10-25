//! Creator declarations retained in source certificates, independent of archive paths.
use std::collections::BTreeMap;

use ctx_history_core::SourceObservation;

use crate::{invalid, retained::OwnedMember, ArchiveIdentity, Result, VerifiedIndex};

pub(crate) const OBSERVATION_KIND: &str = "archive-owned-member-v2";

impl OwnedMember {
    pub(crate) fn decode(observation: &SourceObservation) -> Result<Self> {
        if observation.revision_kind() == crate::retained::OBSERVATION_KIND {
            return Ok(serde_json::from_slice(observation.revision())?);
        }
        if observation.revision_kind() != OBSERVATION_KIND {
            return Err(invalid("unsupported committed archive member reference"));
        }
        let mut bytes = observation.revision();
        let mut read = || -> Result<String> {
            let prefix = bytes
                .get(..2)
                .ok_or_else(|| invalid("truncated archive declaration"))?;
            let size = usize::from(u16::from_be_bytes([prefix[0], prefix[1]]));
            let value = bytes
                .get(2..2 + size)
                .ok_or_else(|| invalid("truncated archive declaration"))?;
            let value = std::str::from_utf8(value)
                .map_err(|_| invalid("invalid archive declaration UTF-8"))?
                .to_owned();
            bytes = &bytes[2 + size..];
            Ok(value)
        };
        let member = Self {
            snapshot: read()?,
            member: read()?,
            identity: Some(ArchiveIdentity {
                origin: read()?,
                view: read()?,
            }),
        };
        if !bytes.is_empty() {
            return Err(invalid("trailing archive declaration bytes"));
        }
        if let Some(identity) = &member.identity {
            identity.validate()?;
        }
        Ok(member)
    }

    /// Length-prefixed UTF-8 stays below the source observation's 4-KiB limit
    /// even for two maximum-length labels consisting entirely of JSON escapes.
    pub(crate) fn encode(&self, identity: &ArchiveIdentity) -> Result<Vec<u8>> {
        identity.validate()?;
        let mut bytes = Vec::new();
        for value in [
            &self.snapshot,
            &self.member,
            &identity.origin,
            &identity.view,
        ] {
            let len =
                u16::try_from(value.len()).map_err(|_| invalid("archive declaration too long"))?;
            bytes.extend_from_slice(&len.to_be_bytes());
            bytes.extend_from_slice(value.as_bytes());
        }
        Ok(bytes)
    }
}

/// Read only metadata belonging to the pinned generation. Older certificates
/// without retained declarations remain readable and omit these labels.
pub fn source_identities(index: &VerifiedIndex) -> Result<BTreeMap<[u8; 32], ArchiveIdentity>> {
    let mut sources = BTreeMap::new();
    for certificate in &index.manifest().sources {
        let observation = certificate.observation();
        if observation.source().source_format() == "ctx_archive_v1"
            && observation.revision_kind() == OBSERVATION_KIND
        {
            if let Some(identity) = OwnedMember::decode(observation)?.identity {
                sources.insert(observation.source().identity().digest(), identity);
            }
        }
    }
    Ok(sources)
}
