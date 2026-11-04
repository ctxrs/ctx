//! Bounded observations of per-record ownership markers. Attribution policy
//! interprets these observations; file layout alone cannot replace them.
use serde::{de::IgnoredAny, Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;

#[derive(Debug, Default, Deserialize, Serialize)]
pub(in super::super) struct ClaudeRecordOwnership {
    #[serde(
        default,
        rename(deserialize = "isSidechain"),
        deserialize_with = "sidechain"
    )]
    sidechain: SidechainObservation,
    #[serde(
        default,
        rename(deserialize = "agentId"),
        deserialize_with = "field_present"
    )]
    agent_id_present: bool,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
enum SidechainObservation {
    #[default]
    Absent,
    False,
    True,
    Other,
}

fn sidechain<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SidechainObservation, D::Error> {
    let raw = <&RawValue>::deserialize(deserializer)?;
    Ok(match raw.get() {
        "false" => SidechainObservation::False,
        "true" => SidechainObservation::True,
        _ => SidechainObservation::Other,
    })
}

fn field_present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    IgnoredAny::deserialize(deserializer)?;
    Ok(true)
}
