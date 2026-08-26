use std::fmt;

use ctx_history_core::{
    LiteralFactKind, ProviderDeclaredFact, MAX_CORE_CONTENT_BYTES, MAX_PROVIDER_DECLARED_FACTS,
};
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

const MAX_RECOGNIZED_FACT_KEYS_PER_OBJECT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum SelectorGroup {
    Type,
    CallId,
    ToolName,
    Arguments,
    Result,
    Protocol,
    Server,
    McpTool,
    Content,
    Invocation,
}

impl SelectorGroup {
    const fn bit(self) -> u16 {
        1_u16 << self as u8
    }
}

#[derive(Debug, Default)]
pub(crate) struct RawJsonAudit {
    duplicate_selectors: u16,
    facts: Vec<ProviderDeclaredFact>,
    facts_available: bool,
}

impl RawJsonAudit {
    pub(crate) fn any_selector_ambiguous(&self) -> bool {
        self.duplicate_selectors != 0
    }

    pub(crate) fn selector_ambiguous(&self, group: SelectorGroup) -> bool {
        self.duplicate_selectors & group.bit() != 0
    }

    pub(crate) fn facts(&self) -> &[ProviderDeclaredFact] {
        if self.facts_available {
            &self.facts
        } else {
            &[]
        }
    }

    fn mark_duplicate(&mut self, group: SelectorGroup) {
        self.duplicate_selectors |= group.bit();
    }

    fn mark_facts_unavailable(&mut self) {
        self.facts_available = false;
        self.facts.clear();
    }

    fn push_fact(&mut self, kind: LiteralFactKind, value: &str) {
        if !self.facts_available || value.is_empty() {
            return;
        }
        if value.len() > MAX_CORE_CONTENT_BYTES || self.facts.len() >= MAX_PROVIDER_DECLARED_FACTS {
            self.mark_facts_unavailable();
            return;
        }
        self.facts.push(ProviderDeclaredFact {
            kind,
            value: value.to_owned(),
        });
    }
}

pub(crate) fn audit_json(
    bytes: &[u8],
    selector_group: fn(&str) -> Option<SelectorGroup>,
    fact_kind: fn(&str) -> Option<LiteralFactKind>,
) -> serde_json::Result<RawJsonAudit> {
    let mut audit = RawJsonAudit {
        facts_available: true,
        ..RawJsonAudit::default()
    };
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    AuditSeed {
        audit: &mut audit,
        selector_group,
        fact_kind,
        direct_fact_kind: None,
    }
    .deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(audit)
}

/// Reports duplicate keys only on selectors consumed by the item-completed
/// projector. Unknown item fields and their arbitrarily shaped JSON remain
/// opaque, so aliases inside tool arguments cannot poison the envelope audit.
pub(crate) fn audit_item_completed_selectors(bytes: &[u8]) -> serde_json::Result<bool> {
    let mut ambiguous = false;
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    ItemCompletedSelectorSeed {
        ambiguous: &mut ambiguous,
        level: ItemCompletedSelectorLevel::Envelope,
    }
    .deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(ambiguous)
}

#[derive(Clone, Copy)]
enum ItemCompletedSelectorLevel {
    Envelope,
    Payload,
    Item,
}

struct ItemCompletedSelectorSeed<'a> {
    ambiguous: &'a mut bool,
    level: ItemCompletedSelectorLevel,
}

impl<'de> DeserializeSeed<'de> for ItemCompletedSelectorSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ItemCompletedSelectorVisitor(self))
    }
}

struct ItemCompletedSelectorVisitor<'a>(ItemCompletedSelectorSeed<'a>);

impl<'de> Visitor<'de> for ItemCompletedSelectorVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex item-completed selector object")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let ItemCompletedSelectorSeed { ambiguous, level } = self.0;
        let mut seen = 0_u16;
        while let Some(key) = map.next_key::<String>()? {
            if let Some(bit) = item_completed_selector_bit(level, &key) {
                if seen & bit != 0 {
                    *ambiguous = true;
                }
                seen |= bit;
            }
            let child_level = match (level, key.as_str()) {
                (ItemCompletedSelectorLevel::Envelope, "payload") => {
                    Some(ItemCompletedSelectorLevel::Payload)
                }
                (ItemCompletedSelectorLevel::Payload, "item") => {
                    Some(ItemCompletedSelectorLevel::Item)
                }
                _ => None,
            };
            if let Some(level) = child_level {
                map.next_value_seed(ItemCompletedSelectorSeed {
                    ambiguous: &mut *ambiguous,
                    level,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }

    fn visit_seq<S>(self, mut sequence: S) -> Result<Self::Value, S::Error>
    where
        S: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(())
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }
}

fn item_completed_selector_bit(level: ItemCompletedSelectorLevel, key: &str) -> Option<u16> {
    match level {
        ItemCompletedSelectorLevel::Envelope => match key {
            "type" => Some(1 << 0),
            "timestamp" => Some(1 << 1),
            "payload" => Some(1 << 2),
            _ => None,
        },
        ItemCompletedSelectorLevel::Payload => match key {
            "type" => Some(1 << 0),
            "thread_id" => Some(1 << 1),
            "turn_id" => Some(1 << 2),
            "item" => Some(1 << 3),
            "started_at_ms" => Some(1 << 4),
            "completed_at_ms" => Some(1 << 5),
            _ => None,
        },
        ItemCompletedSelectorLevel::Item => match key {
            "id" => Some(1 << 0),
            "type" => Some(1 << 1),
            "text" => Some(1 << 2),
            _ => None,
        },
    }
}

struct AuditSeed<'a> {
    audit: &'a mut RawJsonAudit,
    selector_group: fn(&str) -> Option<SelectorGroup>,
    fact_kind: fn(&str) -> Option<LiteralFactKind>,
    direct_fact_kind: Option<LiteralFactKind>,
}

impl<'de> DeserializeSeed<'de> for AuditSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(AuditVisitor(self))
    }
}

struct AuditVisitor<'a>(AuditSeed<'a>);

impl<'de> Visitor<'de> for AuditVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded provider-native JSON value")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let AuditSeed {
            audit,
            selector_group,
            fact_kind,
            ..
        } = self.0;
        let mut seen_selectors = 0_u16;
        let mut seen_fact_keys = Vec::<String>::new();
        while let Some(key) = map.next_key::<String>()? {
            if let Some(group) = selector_group(&key) {
                if seen_selectors & group.bit() != 0 {
                    audit.mark_duplicate(group);
                }
                seen_selectors |= group.bit();
            }
            let direct_fact_kind = fact_kind(&key);
            if direct_fact_kind.is_some() {
                if seen_fact_keys.iter().any(|seen| seen == &key)
                    || seen_fact_keys.len() == MAX_RECOGNIZED_FACT_KEYS_PER_OBJECT
                {
                    audit.mark_facts_unavailable();
                } else {
                    seen_fact_keys.push(key);
                }
            }
            map.next_value_seed(AuditSeed {
                audit: &mut *audit,
                selector_group,
                fact_kind,
                direct_fact_kind,
            })?;
        }
        Ok(())
    }

    fn visit_seq<S>(self, mut sequence: S) -> Result<Self::Value, S::Error>
    where
        S: SeqAccess<'de>,
    {
        let AuditSeed {
            audit,
            selector_group,
            fact_kind,
            direct_fact_kind,
        } = self.0;
        while sequence
            .next_element_seed(AuditSeed {
                audit: &mut *audit,
                selector_group,
                fact_kind,
                direct_fact_kind,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E> {
        if let Some(kind) = self.0.direct_fact_kind {
            self.0.audit.push_fact(kind, value);
        }
        Ok(())
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        if let Some(kind) = self.0.direct_fact_kind {
            self.0.audit.push_fact(kind, value);
        }
        Ok(())
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        if let Some(kind) = self.0.direct_fact_kind {
            self.0.audit.push_fact(kind, &value);
        }
        Ok(())
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        self.0.deserialize(deserializer)
    }
}
