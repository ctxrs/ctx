use serde::de::{IgnoredAny, MapAccess, Visitor};
use tantivy::{schema::Value as TantivyValue, DocAddress, TantivyDocument};

use crate::{fields_from_schema, CoreRecord, IndexError, Result};

/// The one deployed self-contained Core contract that this build may read
/// across a same-epoch additive transition.
pub(crate) const SAME_EPOCH_PREDECESSOR_CORE_FINGERPRINT: &str =
    "c5ad8c7bce69d5fd3f12d3b57e8e49403233db4a74f91882ed649a2bb117b19a";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreContractGeneration {
    Current,
    AllowlistedPredecessor,
}

pub(crate) fn current_core_record_contract_fingerprint() -> String {
    #[cfg(test)]
    if let Some(fingerprint) =
        TEST_CURRENT_CORE_FINGERPRINT.with(|override_value| override_value.borrow().clone())
    {
        return fingerprint;
    }

    ctx_history_core::core_record_contract_fingerprint()
}

pub(crate) fn classify_core_contract_generation(actual: &str) -> Result<CoreContractGeneration> {
    let current = current_core_record_contract_fingerprint();
    if actual == current {
        return Ok(CoreContractGeneration::Current);
    }
    if actual == SAME_EPOCH_PREDECESSOR_CORE_FINGERPRINT {
        return Ok(CoreContractGeneration::AllowlistedPredecessor);
    }
    Err(IndexError::CoreRecordContractMismatch {
        expected: current,
        actual: actual.to_owned(),
    })
}

/// Decodes one stored record according to the manifest-selected Core shape.
///
/// The successor decoder accepts an absent optional attribution member, so an
/// allowlisted fingerprint alone is not enough to prove predecessor shape.
/// The frozen predecessor mode first rejects the successor-only top-level
/// member even when its value is `null`, then applies Core's exact decoder and
/// validation to the remaining record.
pub(crate) fn decode_stored_core_record_for_contract(
    encoded: &[u8],
    contract: CoreContractGeneration,
) -> Result<CoreRecord> {
    if contract == CoreContractGeneration::AllowlistedPredecessor {
        reject_predecessor_successor_member(encoded)?;
    }
    Ok(CoreRecord::decode_stored(encoded)?)
}

/// Audits every live stored record before an allowlisted predecessor reader or
/// migration is made available to normal query materialization.
pub(crate) fn audit_searcher_core_contract(
    searcher: &tantivy::Searcher,
    contract: CoreContractGeneration,
) -> Result<()> {
    if contract == CoreContractGeneration::Current {
        return Ok(());
    }
    let fields = fields_from_schema(searcher.schema())?;
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        for doc_id in 0..segment.max_doc() {
            if segment.is_deleted(doc_id) {
                continue;
            }
            let document: TantivyDocument = searcher.doc(DocAddress::new(
                u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?,
                doc_id,
            ))?;
            let mut values = document.get_all(fields.core_record);
            let encoded = values
                .next()
                .and_then(|value| value.as_bytes())
                .ok_or(IndexError::InvalidStoredDocumentField("core_record"))?;
            if values.next().is_some() {
                return Err(IndexError::InvalidStoredDocumentField("core_record"));
            }
            decode_stored_core_record_for_contract(encoded, contract)?;
        }
    }
    Ok(())
}

fn reject_predecessor_successor_member(encoded: &[u8]) -> Result<()> {
    struct MemberVisitor<'a> {
        successor_member_found: &'a mut bool,
    }

    impl<'de> Visitor<'de> for MemberVisitor<'_> {
        type Value = ();

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a stored Core record object")
        }

        fn visit_map<A>(self, mut map: A) -> std::result::Result<(), A::Error>
        where
            A: MapAccess<'de>,
        {
            while let Some(key) = map.next_key::<String>()? {
                if key == "mcp_tool_call" {
                    *self.successor_member_found = true;
                }
                map.next_value::<IgnoredAny>()?;
            }
            Ok(())
        }
    }

    let mut successor_member_found = false;
    let mut deserializer = serde_json::Deserializer::from_slice(encoded);
    serde::Deserializer::deserialize_map(
        &mut deserializer,
        MemberVisitor {
            successor_member_found: &mut successor_member_found,
        },
    )?;
    deserializer.end()?;
    if successor_member_found {
        return Err(IndexError::PredecessorCoreRecordShapeMismatch);
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static TEST_CURRENT_CORE_FINGERPRINT: std::cell::RefCell<Option<String>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
pub(crate) struct TestCoreFingerprintOverride(Option<String>);

#[cfg(test)]
impl TestCoreFingerprintOverride {
    pub(crate) fn set(fingerprint: impl Into<String>) -> Self {
        let previous = TEST_CURRENT_CORE_FINGERPRINT
            .with(|override_value| override_value.replace(Some(fingerprint.into())));
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for TestCoreFingerprintOverride {
    fn drop(&mut self) {
        TEST_CURRENT_CORE_FINGERPRINT.with(|override_value| override_value.replace(self.0.take()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_contract_allowlist_is_exact_and_fails_closed() {
        let _override = TestCoreFingerprintOverride::set("a".repeat(64));
        assert_eq!(
            classify_core_contract_generation(&"a".repeat(64)).unwrap(),
            CoreContractGeneration::Current
        );
        assert_eq!(
            classify_core_contract_generation(SAME_EPOCH_PREDECESSOR_CORE_FINGERPRINT).unwrap(),
            CoreContractGeneration::AllowlistedPredecessor
        );
        assert!(matches!(
            classify_core_contract_generation(&"b".repeat(64)),
            Err(IndexError::CoreRecordContractMismatch { actual, .. }) if actual == "b".repeat(64)
        ));
    }

    #[test]
    fn predecessor_shape_rejects_successor_member_even_when_null() {
        let source = crate::tests::source("predecessor-shape.jsonl");
        let record = crate::tests::document(&source, 1, "predecessor body");
        let encoded = record.encode_stored().unwrap();
        assert!(decode_stored_core_record_for_contract(
            &encoded,
            CoreContractGeneration::AllowlistedPredecessor
        )
        .is_ok());

        let mut value = serde_json::from_slice::<serde_json::Value>(&encoded).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("mcp_tool_call".to_owned(), serde_json::Value::Null);
        let malformed = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            decode_stored_core_record_for_contract(
                &malformed,
                CoreContractGeneration::AllowlistedPredecessor
            ),
            Err(IndexError::PredecessorCoreRecordShapeMismatch)
        ));
    }
}
