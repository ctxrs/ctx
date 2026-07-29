use serde::{Deserialize, Serialize};

use super::errors::{
    encode_length_prefixed, validate_bytes, validate_nonempty_bytes, validate_text,
    ProjectionContractError, ProjectionContractResult, MAX_KEY_NAMESPACE_BYTES, MAX_LOCATOR_BYTES,
    MAX_LOCATOR_KIND_BYTES, MAX_TYPED_KEY_BYTES, MAX_TYPED_KEY_COMPONENTS,
};

/// Exact provider-native key material.
///
/// Parsing determines the storage type. Identity encoding does not trim,
/// normalize, case-fold, stringify, or otherwise reinterpret values.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TypedKey {
    Null,
    Bytes(Vec<u8>),
    Utf8(String),
    I64(i64),
    U64(u64),
    F64Bits(u64),
    Bool(bool),
    Composite(Vec<TypedKey>),
}

impl TypedKey {
    pub fn bytes(value: Vec<u8>) -> ProjectionContractResult<Self> {
        validate_bytes("typed_key_bytes", &value, MAX_TYPED_KEY_BYTES)?;
        Ok(Self::Bytes(value))
    }

    pub fn utf8(value: impl Into<String>) -> ProjectionContractResult<Self> {
        let value = value.into();
        validate_text("typed_key_utf8", &value, MAX_TYPED_KEY_BYTES)?;
        Ok(Self::Utf8(value))
    }

    pub fn composite(values: Vec<Self>) -> ProjectionContractResult<Self> {
        if values.len() > MAX_TYPED_KEY_COMPONENTS {
            return Err(ProjectionContractError::TooManyKeyComponents {
                actual: values.len(),
                maximum: MAX_TYPED_KEY_COMPONENTS,
            });
        }
        let mut encoded = Vec::new();
        encode_typed_key(&mut encoded, &Self::Composite(values.clone()))?;
        validate_bytes("typed_composite_key", &encoded, MAX_TYPED_KEY_BYTES)?;
        Ok(Self::Composite(values))
    }

    pub fn from_f64(value: f64) -> Self {
        Self::F64Bits(value.to_bits())
    }
}

/// Hydration/citation evidence. A locator is intentionally not identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeLocator {
    kind: String,
    value: Vec<u8>,
}

impl NativeLocator {
    pub fn new(kind: impl Into<String>, value: Vec<u8>) -> ProjectionContractResult<Self> {
        let locator = Self {
            kind: kind.into(),
            value,
        };
        validate_text("native_locator_kind", &locator.kind, MAX_LOCATOR_KIND_BYTES)?;
        validate_nonempty_bytes("native_locator", &locator.value, MAX_LOCATOR_BYTES)?;
        Ok(locator)
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PositionStability {
    AppendStable,
    StableSlot,
    RevisionScoped,
}

/// Provider-native identity for one logical session.
///
/// Native IDs and composites are durable across source revisions. A positional
/// key must instead declare the provider guarantee that makes its coordinate
/// safe to reuse.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NativeSessionKey {
    NativeId {
        namespace: String,
        value: TypedKey,
    },
    Composite {
        namespace: String,
        parts: Vec<TypedKey>,
    },
    CertifiedPosition {
        kind: String,
        coordinate: TypedKey,
        stability: PositionStability,
        revision_scope: Option<TypedKey>,
    },
}

impl NativeSessionKey {
    pub fn native_id(
        namespace: impl Into<String>,
        value: TypedKey,
    ) -> ProjectionContractResult<Self> {
        let key = Self::NativeId {
            namespace: namespace.into(),
            value,
        };
        key.validate_contract()?;
        Ok(key)
    }

    pub fn composite(
        namespace: impl Into<String>,
        parts: Vec<TypedKey>,
    ) -> ProjectionContractResult<Self> {
        let key = Self::Composite {
            namespace: namespace.into(),
            parts,
        };
        key.validate_contract()?;
        Ok(key)
    }

    pub fn certified_position(
        kind: impl Into<String>,
        coordinate: TypedKey,
        stability: PositionStability,
    ) -> ProjectionContractResult<Self> {
        if stability == PositionStability::RevisionScoped {
            return Err(ProjectionContractError::RevisionScopeRequired);
        }
        let key = Self::CertifiedPosition {
            kind: kind.into(),
            coordinate,
            stability,
            revision_scope: None,
        };
        key.validate_contract()?;
        Ok(key)
    }

    /// A session position that is stable only within one provider/source
    /// revision.
    pub fn revision_scoped_position(
        kind: impl Into<String>,
        coordinate: TypedKey,
        revision_scope: TypedKey,
    ) -> ProjectionContractResult<Self> {
        let key = Self::CertifiedPosition {
            kind: kind.into(),
            coordinate,
            stability: PositionStability::RevisionScoped,
            revision_scope: Some(revision_scope),
        };
        key.validate_contract()?;
        Ok(key)
    }

    pub fn validate_contract(&self) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        encode_native_session_key(&mut encoded, self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NativeItemKey {
    NativeId {
        namespace: String,
        value: TypedKey,
    },
    Composite {
        namespace: String,
        parts: Vec<TypedKey>,
    },
    CertifiedPosition {
        kind: String,
        coordinate: TypedKey,
        stability: PositionStability,
        revision_scope: Option<TypedKey>,
    },
}

impl NativeItemKey {
    pub fn native_id(
        namespace: impl Into<String>,
        value: TypedKey,
    ) -> ProjectionContractResult<Self> {
        let key = Self::NativeId {
            namespace: namespace.into(),
            value,
        };
        key.validate_contract()?;
        Ok(key)
    }

    pub fn composite(
        namespace: impl Into<String>,
        parts: Vec<TypedKey>,
    ) -> ProjectionContractResult<Self> {
        let key = Self::Composite {
            namespace: namespace.into(),
            parts,
        };
        key.validate_contract()?;
        Ok(key)
    }

    pub fn certified_position(
        kind: impl Into<String>,
        coordinate: TypedKey,
        stability: PositionStability,
    ) -> ProjectionContractResult<Self> {
        if stability == PositionStability::RevisionScoped {
            return Err(ProjectionContractError::RevisionScopeRequired);
        }
        let key = Self::CertifiedPosition {
            kind: kind.into(),
            coordinate,
            stability,
            revision_scope: None,
        };
        key.validate_contract()?;
        Ok(key)
    }

    /// A position that is stable only within one provider/source revision.
    ///
    /// The scope must be a provider-native snapshot/generation key known before
    /// projection. It is explicit so a rewrite cannot accidentally reuse an
    /// ordinal from an earlier snapshot.
    pub fn revision_scoped_position(
        kind: impl Into<String>,
        coordinate: TypedKey,
        revision_scope: TypedKey,
    ) -> ProjectionContractResult<Self> {
        let key = Self::CertifiedPosition {
            kind: kind.into(),
            coordinate,
            stability: PositionStability::RevisionScoped,
            revision_scope: Some(revision_scope),
        };
        key.validate_contract()?;
        Ok(key)
    }

    pub fn validate_contract(&self) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        encode_native_item_key(&mut encoded, self)
    }
}

/// Provider-native selector for one logical subrecord within a native item.
///
/// Absence means the event represents the whole native item. A present
/// positional selector must declare why its coordinate is stable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SubrecordSelector {
    NativeId {
        namespace: String,
        value: TypedKey,
    },
    Composite {
        namespace: String,
        parts: Vec<TypedKey>,
    },
    CertifiedPosition {
        kind: String,
        coordinate: TypedKey,
        stability: PositionStability,
        revision_scope: Option<TypedKey>,
    },
}

impl SubrecordSelector {
    pub fn native_id(
        namespace: impl Into<String>,
        value: TypedKey,
    ) -> ProjectionContractResult<Self> {
        let selector = Self::NativeId {
            namespace: namespace.into(),
            value,
        };
        selector.validate_contract()?;
        Ok(selector)
    }

    pub fn composite(
        namespace: impl Into<String>,
        parts: Vec<TypedKey>,
    ) -> ProjectionContractResult<Self> {
        let selector = Self::Composite {
            namespace: namespace.into(),
            parts,
        };
        selector.validate_contract()?;
        Ok(selector)
    }

    pub fn certified_position(
        kind: impl Into<String>,
        coordinate: TypedKey,
        stability: PositionStability,
    ) -> ProjectionContractResult<Self> {
        if stability == PositionStability::RevisionScoped {
            return Err(ProjectionContractError::RevisionScopeRequired);
        }
        let selector = Self::CertifiedPosition {
            kind: kind.into(),
            coordinate,
            stability,
            revision_scope: None,
        };
        selector.validate_contract()?;
        Ok(selector)
    }

    /// A subrecord position that is stable only within one provider/source
    /// revision.
    pub fn revision_scoped_position(
        kind: impl Into<String>,
        coordinate: TypedKey,
        revision_scope: TypedKey,
    ) -> ProjectionContractResult<Self> {
        let selector = Self::CertifiedPosition {
            kind: kind.into(),
            coordinate,
            stability: PositionStability::RevisionScoped,
            revision_scope: Some(revision_scope),
        };
        selector.validate_contract()?;
        Ok(selector)
    }

    pub fn validate_contract(&self) -> ProjectionContractResult<()> {
        let mut encoded = Vec::new();
        encode_subrecord_selector(&mut encoded, self)
    }
}

enum IdentityKeyRef<'a> {
    NativeId {
        namespace: &'a str,
        value: &'a TypedKey,
    },
    Composite {
        namespace: &'a str,
        parts: &'a [TypedKey],
    },
    CertifiedPosition {
        kind: &'a str,
        coordinate: &'a TypedKey,
        stability: PositionStability,
        revision_scope: Option<&'a TypedKey>,
    },
}

pub(super) fn encode_native_session_key(
    target: &mut Vec<u8>,
    key: &NativeSessionKey,
) -> ProjectionContractResult<()> {
    let key = match key {
        NativeSessionKey::NativeId { namespace, value } => {
            IdentityKeyRef::NativeId { namespace, value }
        }
        NativeSessionKey::Composite { namespace, parts } => {
            IdentityKeyRef::Composite { namespace, parts }
        }
        NativeSessionKey::CertifiedPosition {
            kind,
            coordinate,
            stability,
            revision_scope,
        } => IdentityKeyRef::CertifiedPosition {
            kind,
            coordinate,
            stability: *stability,
            revision_scope: revision_scope.as_ref(),
        },
    };
    encode_identity_key(
        target,
        key,
        "native_session_namespace",
        "native_session_position_kind",
    )
}

pub(super) fn encode_native_item_key(
    target: &mut Vec<u8>,
    key: &NativeItemKey,
) -> ProjectionContractResult<()> {
    let key = match key {
        NativeItemKey::NativeId { namespace, value } => {
            IdentityKeyRef::NativeId { namespace, value }
        }
        NativeItemKey::Composite { namespace, parts } => {
            IdentityKeyRef::Composite { namespace, parts }
        }
        NativeItemKey::CertifiedPosition {
            kind,
            coordinate,
            stability,
            revision_scope,
        } => IdentityKeyRef::CertifiedPosition {
            kind,
            coordinate,
            stability: *stability,
            revision_scope: revision_scope.as_ref(),
        },
    };
    encode_identity_key(target, key, "native_item_namespace", "native_position_kind")
}

pub(super) fn encode_subrecord_selector(
    target: &mut Vec<u8>,
    selector: &SubrecordSelector,
) -> ProjectionContractResult<()> {
    let selector = match selector {
        SubrecordSelector::NativeId { namespace, value } => {
            IdentityKeyRef::NativeId { namespace, value }
        }
        SubrecordSelector::Composite { namespace, parts } => {
            IdentityKeyRef::Composite { namespace, parts }
        }
        SubrecordSelector::CertifiedPosition {
            kind,
            coordinate,
            stability,
            revision_scope,
        } => IdentityKeyRef::CertifiedPosition {
            kind,
            coordinate,
            stability: *stability,
            revision_scope: revision_scope.as_ref(),
        },
    };
    encode_identity_key(
        target,
        selector,
        "subrecord_namespace",
        "subrecord_position_kind",
    )
}

fn encode_identity_key(
    target: &mut Vec<u8>,
    key: IdentityKeyRef<'_>,
    namespace_field: &'static str,
    position_kind_field: &'static str,
) -> ProjectionContractResult<()> {
    match key {
        IdentityKeyRef::NativeId { namespace, value } => {
            validate_text(namespace_field, namespace, MAX_KEY_NAMESPACE_BYTES)?;
            target.push(1);
            encode_length_prefixed(target, namespace.as_bytes());
            encode_typed_key(target, value)?;
        }
        IdentityKeyRef::Composite { namespace, parts } => {
            validate_text(namespace_field, namespace, MAX_KEY_NAMESPACE_BYTES)?;
            if parts.len() > MAX_TYPED_KEY_COMPONENTS {
                return Err(ProjectionContractError::TooManyKeyComponents {
                    actual: parts.len(),
                    maximum: MAX_TYPED_KEY_COMPONENTS,
                });
            }
            target.push(2);
            encode_length_prefixed(target, namespace.as_bytes());
            target.extend_from_slice(&(parts.len() as u32).to_be_bytes());
            for part in parts {
                encode_typed_key(target, part)?;
            }
        }
        IdentityKeyRef::CertifiedPosition {
            kind,
            coordinate,
            stability,
            revision_scope,
        } => {
            validate_text(position_kind_field, kind, MAX_KEY_NAMESPACE_BYTES)?;
            match (stability, revision_scope) {
                (PositionStability::RevisionScoped, None) => {
                    return Err(ProjectionContractError::RevisionScopeRequired);
                }
                (PositionStability::AppendStable | PositionStability::StableSlot, Some(_)) => {
                    return Err(ProjectionContractError::UnexpectedRevisionScope)
                }
                _ => {}
            }
            target.push(3);
            encode_length_prefixed(target, kind.as_bytes());
            target.push(match stability {
                PositionStability::AppendStable => 1,
                PositionStability::StableSlot => 2,
                PositionStability::RevisionScoped => 3,
            });
            encode_typed_key(target, coordinate)?;
            if let Some(scope) = revision_scope {
                encode_typed_key(target, scope)?;
            }
        }
    }
    Ok(())
}

pub(super) fn encode_typed_key(
    target: &mut Vec<u8>,
    key: &TypedKey,
) -> ProjectionContractResult<()> {
    match key {
        TypedKey::Null => target.push(0),
        TypedKey::Bytes(value) => {
            validate_bytes("typed_key_bytes", value, MAX_TYPED_KEY_BYTES)?;
            target.push(1);
            encode_length_prefixed(target, value);
        }
        TypedKey::Utf8(value) => {
            validate_text("typed_key_utf8", value, MAX_TYPED_KEY_BYTES)?;
            target.push(2);
            encode_length_prefixed(target, value.as_bytes());
        }
        TypedKey::I64(value) => {
            target.push(3);
            target.extend_from_slice(&value.to_be_bytes());
        }
        TypedKey::U64(value) => {
            target.push(4);
            target.extend_from_slice(&value.to_be_bytes());
        }
        TypedKey::F64Bits(value) => {
            target.push(5);
            target.extend_from_slice(&value.to_be_bytes());
        }
        TypedKey::Bool(value) => {
            target.push(6);
            target.push(u8::from(*value));
        }
        TypedKey::Composite(values) => {
            if values.len() > MAX_TYPED_KEY_COMPONENTS {
                return Err(ProjectionContractError::TooManyKeyComponents {
                    actual: values.len(),
                    maximum: MAX_TYPED_KEY_COMPONENTS,
                });
            }
            target.push(7);
            target.extend_from_slice(&(values.len() as u32).to_be_bytes());
            for value in values {
                encode_typed_key(target, value)?;
            }
        }
    }
    Ok(())
}
