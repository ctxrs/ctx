use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use tantivy::{schema::Value as TantivyValue, DocAddress, TantivyDocument};

use crate::{
    current_source_generation_policy_hash, fields_from_schema, CoreRecord, IndexError, Result,
};

/// The one deployed self-contained Core contract that this build may read
/// across a same-epoch additive transition.
pub(crate) const SAME_EPOCH_PREDECESSOR_CORE_FINGERPRINT: &str =
    "7552eee7cae0695a98f202b02f52cbf5680845cb7bacea4ed754e283bc15f051";
/// The exact source-generation policy carried by that deployed predecessor.
pub(crate) const SAME_EPOCH_PREDECESSOR_SOURCE_GENERATION_POLICY_HASH: &str =
    "e728b5d7b76d04248e9dccc91fc11d915fcbcd714b445090725ba0604b8e8b37";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreContractGeneration {
    Current,
    AllowlistedPredecessor,
}

pub(crate) fn current_core_record_contract_fingerprint() -> String {
    ctx_history_core::core_record_contract_fingerprint()
}

pub(crate) fn classify_core_contract_generation(actual: &str) -> Result<CoreContractGeneration> {
    let current = current_core_record_contract_fingerprint();
    if actual == current {
        return Ok(CoreContractGeneration::Current);
    }
    Err(IndexError::CoreRecordContractMismatch {
        expected: current,
        actual: actual.to_owned(),
    })
}

/// Returns the only source-generation policy authorized for this Core shape.
///
/// The predecessor pair is immutable deployed evidence. Current generations
/// always use the policy derived from the current Core revisions.
pub(crate) fn expected_source_generation_policy_hash(
    contract: CoreContractGeneration,
) -> Result<String> {
    match contract {
        CoreContractGeneration::Current => Ok(current_source_generation_policy_hash()?),
        CoreContractGeneration::AllowlistedPredecessor => {
            Ok(SAME_EPOCH_PREDECESSOR_SOURCE_GENERATION_POLICY_HASH.to_owned())
        }
    }
}

/// Decodes one stored record according to the manifest-selected Core shape.
///
/// The predecessor already supports exact MCP tool-call attribution. The
/// successor decoder also accepts an absent exchange, so an allowlisted
/// fingerprint alone is not enough to prove predecessor shape. The frozen
/// predecessor mode first rejects the successor-only nested MCP exchange
/// member even when its value is null, then applies Core's exact decoder and
/// validation to the remaining record.
pub(crate) fn decode_stored_core_record_for_contract(
    encoded: &[u8],
    contract: CoreContractGeneration,
) -> Result<CoreRecord> {
    if contract == CoreContractGeneration::AllowlistedPredecessor {
        reject_predecessor_successor_members(encoded)?;
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

fn reject_predecessor_successor_members(encoded: &[u8]) -> Result<()> {
    struct MemberVisitor<'a> {
        successor_member: &'a mut Option<&'static str>,
    }

    struct ContentMemberSeed<'a> {
        successor_member: &'a mut Option<&'static str>,
    }

    struct ContentMemberVisitor<'a> {
        successor_member: &'a mut Option<&'static str>,
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
                if key == "content" {
                    map.next_value_seed(ContentMemberSeed {
                        successor_member: self.successor_member,
                    })?;
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(())
        }
    }

    impl<'de> DeserializeSeed<'de> for ContentMemberSeed<'_> {
        type Value = ();

        fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_map(ContentMemberVisitor {
                successor_member: self.successor_member,
            })
        }
    }

    impl<'de> Visitor<'de> for ContentMemberVisitor<'_> {
        type Value = ();

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a stored Core content object")
        }

        fn visit_map<A>(self, mut map: A) -> std::result::Result<(), A::Error>
        where
            A: MapAccess<'de>,
        {
            while let Some(key) = map.next_key::<String>()? {
                if key == "mcp_exchange" {
                    self.successor_member.get_or_insert("content.mcp_exchange");
                }
                map.next_value::<IgnoredAny>()?;
            }
            Ok(())
        }
    }

    let mut successor_member = None;
    let mut deserializer = serde_json::Deserializer::from_slice(encoded);
    serde::Deserializer::deserialize_map(
        &mut deserializer,
        MemberVisitor {
            successor_member: &mut successor_member,
        },
    )?;
    deserializer.end()?;
    if let Some(member) = successor_member {
        return Err(IndexError::PredecessorCoreRecordShapeMismatch { member });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_predecessor_contract_fails_closed() {
        let current = current_core_record_contract_fingerprint();
        assert_eq!(
            classify_core_contract_generation(&current).unwrap(),
            CoreContractGeneration::Current
        );
        assert!(matches!(
            classify_core_contract_generation(SAME_EPOCH_PREDECESSOR_CORE_FINGERPRINT),
            Err(IndexError::CoreRecordContractMismatch { actual, .. })
                if actual == SAME_EPOCH_PREDECESSOR_CORE_FINGERPRINT
        ));
        assert!(matches!(
            classify_core_contract_generation(&"b".repeat(64)),
            Err(IndexError::CoreRecordContractMismatch { actual, .. }) if actual == "b".repeat(64)
        ));
        assert_eq!(
            expected_source_generation_policy_hash(CoreContractGeneration::Current).unwrap(),
            current_source_generation_policy_hash().unwrap()
        );
        assert_eq!(
            expected_source_generation_policy_hash(CoreContractGeneration::AllowlistedPredecessor)
                .unwrap(),
            SAME_EPOCH_PREDECESSOR_SOURCE_GENERATION_POLICY_HASH
        );
        assert_ne!(
            current_source_generation_policy_hash().unwrap(),
            SAME_EPOCH_PREDECESSOR_SOURCE_GENERATION_POLICY_HASH
        );
    }

    #[test]
    fn predecessor_shape_accepts_attribution_and_rejects_successor_exchange() {
        let source = crate::tests::source("predecessor-shape.jsonl");
        let mut record = crate::tests::document(&source, 1, "predecessor body");
        record.mcp_tool_call = Some(ctx_history_core::McpToolCallAttribution {
            server: "fixture-server".to_owned(),
            tool: "fixture-tool".to_owned(),
        });
        let encoded = record.encode_stored().unwrap();
        assert!(decode_stored_core_record_for_contract(
            &encoded,
            CoreContractGeneration::AllowlistedPredecessor
        )
        .is_ok());

        for malformed in [
            {
                let mut value = serde_json::from_slice::<serde_json::Value>(&encoded).unwrap();
                value["content"]
                    .as_object_mut()
                    .unwrap()
                    .insert("mcp_exchange".to_owned(), serde_json::Value::Null);
                value
            },
            {
                let mut value = serde_json::from_slice::<serde_json::Value>(&encoded).unwrap();
                value["content"].as_object_mut().unwrap().insert(
                    "mcp_exchange".to_owned(),
                    serde_json::json!({"provider_call_id": "successor-call"}),
                );
                value
            },
        ] {
            let malformed = serde_json::to_vec(&malformed).unwrap();
            assert!(matches!(
                decode_stored_core_record_for_contract(
                    &malformed,
                    CoreContractGeneration::AllowlistedPredecessor
                ),
                Err(IndexError::PredecessorCoreRecordShapeMismatch { member: actual })
                    if actual == "content.mcp_exchange"
            ));
        }
    }

    #[test]
    fn predecessor_shape_rejects_escaped_and_duplicate_nested_successor_keys() {
        fn insert_after(encoded: &[u8], marker: &[u8], member: &[u8]) -> Vec<u8> {
            let offset = encoded
                .windows(marker.len())
                .position(|window| window == marker)
                .unwrap()
                + marker.len();
            let mut malformed = Vec::with_capacity(encoded.len() + member.len());
            malformed.extend_from_slice(&encoded[..offset]);
            malformed.extend_from_slice(member);
            malformed.extend_from_slice(&encoded[offset..]);
            malformed
        }

        let source = crate::tests::source("predecessor-nested-duplicates.jsonl");
        let record = crate::tests::document(&source, 1, "predecessor body");
        let encoded = record.encode_stored().unwrap();
        for member in [
            br#""mcp\u005fexchange":null,"#.as_slice(),
            br#""mcp_exchange":null,"mcp_exchange":null,"#.as_slice(),
        ] {
            let malformed = insert_after(&encoded, br#""content":{"#, member);
            assert!(matches!(
                decode_stored_core_record_for_contract(
                    &malformed,
                    CoreContractGeneration::AllowlistedPredecessor
                ),
                Err(IndexError::PredecessorCoreRecordShapeMismatch {
                    member: "content.mcp_exchange"
                })
            ));
        }
    }
}
