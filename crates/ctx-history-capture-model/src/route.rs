use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum encoded size of one provider-defined route role.
pub const MAX_PROVIDER_ROUTE_ROLE_BYTES: usize = 256;

/// Provider-defined role of one physical route emitted by discovery.
///
/// Static roles preserve their exact released bytes. Dynamic roles occupy a
/// disjoint NUL-prefixed namespace and length-frame every component, so
/// different component boundaries cannot collide.
#[derive(Debug, Clone)]
pub struct ProviderRouteRole(ProviderRouteRoleStorage);

#[derive(Debug, Clone)]
enum ProviderRouteRoleStorage {
    Static(&'static [u8]),
    Dynamic(Vec<u8>),
}

/// A dynamic provider route role exceeded its fixed encoded bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("provider route role exceeds the {MAX_PROVIDER_ROUTE_ROLE_BYTES}-byte limit")]
pub struct ProviderRouteRoleError;

impl ProviderRouteRole {
    /// Creates a role from a compile-time provider contract.
    ///
    /// Static role bytes must be nonempty, bounded, and contain no NUL byte.
    /// The NUL exclusion keeps the static and dynamic namespaces disjoint.
    pub const fn from_static(value: &'static str) -> Self {
        let bytes = value.as_bytes();
        assert!(!bytes.is_empty());
        assert!(bytes.len() <= MAX_PROVIDER_ROUTE_ROLE_BYTES);
        let mut index = 0;
        while index < bytes.len() {
            assert!(bytes[index] != 0);
            index += 1;
        }
        Self(ProviderRouteRoleStorage::Static(bytes))
    }

    /// Creates a collision-safe dynamic role from exact byte components.
    ///
    /// The encoding is one NUL namespace marker followed by repeated
    /// big-endian u64 length and value frames. Empty components and an empty
    /// component list remain distinct.
    pub fn from_dynamic<I, B>(components: I) -> Result<Self, ProviderRouteRoleError>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        let mut encoded = vec![0];
        for component in components {
            let component = component.as_ref();
            let length = u64::try_from(component.len()).map_err(|_| ProviderRouteRoleError)?;
            let next_len = encoded
                .len()
                .checked_add(std::mem::size_of::<u64>())
                .and_then(|length| length.checked_add(component.len()))
                .ok_or(ProviderRouteRoleError)?;
            if next_len > MAX_PROVIDER_ROUTE_ROLE_BYTES {
                return Err(ProviderRouteRoleError);
            }
            encoded.extend_from_slice(&length.to_be_bytes());
            encoded.extend_from_slice(component);
        }
        Ok(Self(ProviderRouteRoleStorage::Dynamic(encoded)))
    }

    pub fn as_bytes(&self) -> &[u8] {
        match &self.0 {
            ProviderRouteRoleStorage::Static(value) => value,
            ProviderRouteRoleStorage::Dynamic(value) => value,
        }
    }

    /// Reconstructs one exact role from a persisted bounded encoding.
    ///
    /// Static roles are nonempty and contain no NUL. Dynamic roles begin with
    /// a NUL namespace marker followed by complete big-endian u64 frames.
    /// The returned owned representation deliberately compares by encoded
    /// bytes, so it remains equal to the corresponding static role.
    pub fn try_from_encoded(value: &[u8]) -> Result<Self, ProviderRouteRoleError> {
        if value.is_empty() || value.len() > MAX_PROVIDER_ROUTE_ROLE_BYTES {
            return Err(ProviderRouteRoleError);
        }
        if value[0] != 0 {
            if value.contains(&0) {
                return Err(ProviderRouteRoleError);
            }
        } else {
            let mut offset = 1;
            while offset < value.len() {
                let Some(length_end) = offset.checked_add(std::mem::size_of::<u64>()) else {
                    return Err(ProviderRouteRoleError);
                };
                let Some(length_bytes) = value.get(offset..length_end) else {
                    return Err(ProviderRouteRoleError);
                };
                let length = u64::from_be_bytes(
                    length_bytes
                        .try_into()
                        .map_err(|_| ProviderRouteRoleError)?,
                );
                let length = usize::try_from(length).map_err(|_| ProviderRouteRoleError)?;
                offset = length_end
                    .checked_add(length)
                    .ok_or(ProviderRouteRoleError)?;
                if offset > value.len() {
                    return Err(ProviderRouteRoleError);
                }
            }
        }
        Ok(Self(ProviderRouteRoleStorage::Dynamic(value.to_vec())))
    }
}

impl PartialEq for ProviderRouteRole {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for ProviderRouteRole {}

impl Hash for ProviderRouteRole {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_bytes().hash(state);
    }
}

/// Exact identity of one selected ingestion route.
///
/// The digest is derived by discovery from the provider, format, selection
/// authority, and exact local route locator; paths themselves do not enter
/// Core or Pro records. Deserialization remains transparent and deliberately
/// defers validation so persisted corruption reaches the owning format layer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceRouteIdentity(String);

/// A source-route identity was not exactly one lowercase SHA-256 digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("source route identity is not exactly 64 lowercase hexadecimal characters")]
pub struct SourceRouteIdentityError;

impl SourceRouteIdentity {
    pub fn from_sha256(value: String) -> Result<Self, SourceRouteIdentityError> {
        let identity = Self(value);
        identity.validate()?;
        Ok(identity)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Validates the persisted route identity after deserialization.
    pub fn validate(&self) -> Result<(), SourceRouteIdentityError> {
        if is_lowercase_sha256(&self.0) {
            Ok(())
        } else {
            Err(SourceRouteIdentityError)
        }
    }
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_identity_preserves_transparent_wire_form_and_validates_sha256() {
        let value = "ab".repeat(32);
        let identity = SourceRouteIdentity::from_sha256(value.clone()).unwrap();

        assert_eq!(identity.as_str(), value);
        assert_eq!(
            serde_json::to_string(&identity).unwrap(),
            format!("\"{value}\"")
        );
        assert!(identity.validate().is_ok());
        assert_eq!(
            SourceRouteIdentity::from_sha256("AB".repeat(32)),
            Err(SourceRouteIdentityError)
        );
        assert_eq!(
            SourceRouteIdentity::from_sha256("a".repeat(63)),
            Err(SourceRouteIdentityError)
        );
    }

    #[test]
    fn route_identity_deserialization_defers_validation() {
        let malformed: SourceRouteIdentity =
            serde_json::from_str(&format!("\"{}\"", "AB".repeat(32))).unwrap();

        assert_eq!(malformed.as_str(), "AB".repeat(32));
        assert_eq!(malformed.validate(), Err(SourceRouteIdentityError));
    }

    #[test]
    fn provider_route_roles_preserve_static_bytes_and_frame_dynamic_components() {
        const RELEASED: ProviderRouteRole =
            ProviderRouteRole::from_static("codex-archived-sessions");
        assert_eq!(RELEASED.as_bytes(), b"codex-archived-sessions");

        let split = ProviderRouteRole::from_dynamic([b"a".as_slice(), b"bc".as_slice()]).unwrap();
        let joined = ProviderRouteRole::from_dynamic([b"ab".as_slice(), b"c".as_slice()]).unwrap();
        assert_ne!(split, joined);
        assert_eq!(split.as_bytes()[0], 0);
        assert_ne!(
            split.as_bytes(),
            ProviderRouteRole::from_static("a-bc").as_bytes()
        );
    }

    #[test]
    fn provider_route_roles_enforce_the_shared_bound() {
        assert_eq!(
            ProviderRouteRole::from_dynamic([vec![b'x'; MAX_PROVIDER_ROUTE_ROLE_BYTES]]),
            Err(ProviderRouteRoleError)
        );
        assert!(ProviderRouteRole::from_dynamic(std::iter::empty::<&[u8]>()).is_ok());
        assert_ne!(
            ProviderRouteRole::from_dynamic(std::iter::empty::<&[u8]>()).unwrap(),
            ProviderRouteRole::from_dynamic([b"".as_slice()]).unwrap()
        );
    }

    #[test]
    fn encoded_provider_route_roles_are_strict_and_compare_by_exact_bytes() {
        let static_role = ProviderRouteRole::from_static("released-role");
        assert_eq!(
            ProviderRouteRole::try_from_encoded(b"released-role").unwrap(),
            static_role
        );
        let dynamic =
            ProviderRouteRole::from_dynamic([b"one".as_slice(), b"two".as_slice()]).unwrap();
        assert_eq!(
            ProviderRouteRole::try_from_encoded(dynamic.as_bytes()).unwrap(),
            dynamic
        );
        assert!(ProviderRouteRole::try_from_encoded(b"").is_err());
        assert!(ProviderRouteRole::try_from_encoded(b"static\0role").is_err());
        assert!(ProviderRouteRole::try_from_encoded(&[0, 0, 0]).is_err());
        assert!(ProviderRouteRole::try_from_encoded(&[0, 0, 0, 0, 0, 0, 0, 0, 1]).is_err());
    }
}
