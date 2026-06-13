use std::collections::{HashMap, HashSet};

use serde::Deserialize;

const DEFAULT_REASONING_EFFORT: &str = "medium";
const KNOWN_EFFORT_IDS: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];

#[derive(Debug, Clone)]
struct ModelInfo {
    base: String,
    effort: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ModelCatalog {
    full_ids: Vec<String>,
    current_model_id: Option<String>,
    base_ids: Vec<String>,
    efforts_by_base: HashMap<String, Vec<String>>,
    full_id_by_base_effort: HashMap<String, HashMap<String, String>>,
    info_by_full_id: HashMap<String, ModelInfo>,
}

impl ModelCatalog {
    pub fn full_ids(&self) -> &[String] {
        &self.full_ids
    }

    pub fn current_model_id(&self) -> Option<&str> {
        self.current_model_id.as_deref()
    }

    pub fn default_model_id(&self) -> Option<&str> {
        self.current_model_id()
            .or_else(|| self.full_ids.first().map(String::as_str))
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    pub full_model_id: String,
}

pub fn normalize_effort_id(value: &str) -> String {
    let raw = value.trim().to_lowercase();
    match raw.as_str() {
        "extra_high" | "extra-high" | "extra high" | "extrahigh" => "xhigh".to_string(),
        _ => raw,
    }
}

pub fn deserialize_optional_reasoning_effort<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(raw) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let normalized = normalize_effort_id(trimmed);
    if !KNOWN_EFFORT_IDS.contains(&normalized.as_str()) {
        return Err(serde::de::Error::custom(format!(
            "invalid reasoning_effort '{trimmed}'"
        )));
    }
    Ok(Some(normalized))
}

fn split_model_id(full: &str) -> (String, Option<String>) {
    let trimmed = full.trim();
    if trimmed.is_empty() {
        return (String::new(), None);
    }
    if let Some(idx) = trimmed.rfind('/') {
        if idx > 0 && idx + 1 < trimmed.len() {
            let base = trimmed[..idx].to_string();
            let suffix = trimmed[idx + 1..].trim().to_string();
            if !suffix.is_empty() {
                return (base, Some(suffix));
            }
        }
    }
    (trimmed.to_string(), None)
}

fn is_known_effort_id(value: &str) -> bool {
    let norm = normalize_effort_id(value);
    KNOWN_EFFORT_IDS.iter().any(|id| *id == norm)
}

fn has_trailing_paren_suffix(name: &str, suffix: &str) -> bool {
    let trimmed = name.trim_end();
    if !trimmed.ends_with(')') {
        return false;
    }
    let Some(start) = trimmed.rfind('(') else {
        return false;
    };
    let inner = trimmed[start + 1..trimmed.len() - 1].trim();
    normalize_effort_id(inner) == normalize_effort_id(suffix)
}

fn order_effort_ids(list: &mut [String]) {
    let order_index = |value: &str| {
        let norm = normalize_effort_id(value);
        KNOWN_EFFORT_IDS
            .iter()
            .position(|id| *id == norm)
            .unwrap_or(usize::MAX)
    };
    list.sort_by(|a, b| {
        let ia = order_index(a);
        let ib = order_index(b);
        if ia != ib {
            return ia.cmp(&ib);
        }
        a.cmp(b)
    });
}

pub fn build_model_catalog(models: &serde_json::Value) -> Option<ModelCatalog> {
    let entries = extract_model_entries(models);
    if entries.is_empty() {
        return None;
    }
    let current_model_id = models
        .get("current_model_id")
        .or_else(|| models.get("currentModelId"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut full_ids = HashSet::new();
    let mut base_ids = HashSet::new();
    let mut info_by_full_id = HashMap::new();
    let mut raw_efforts_by_base: HashMap<String, HashSet<String>> = HashMap::new();
    let mut full_id_by_base_effort: HashMap<String, HashMap<String, String>> = HashMap::new();

    for (id, name) in entries {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (base_candidate, suffix) = split_model_id(trimmed);
        let effort = suffix.and_then(|s| {
            if is_known_effort_id(&s)
                || name
                    .as_deref()
                    .map(|n| has_trailing_paren_suffix(n, &s))
                    .unwrap_or(false)
            {
                Some(s)
            } else {
                None
            }
        });
        let base = if effort.is_some() {
            base_candidate
        } else {
            trimmed.to_string()
        };
        base_ids.insert(base.clone());
        full_ids.insert(trimmed.to_string());
        info_by_full_id.insert(
            trimmed.to_string(),
            ModelInfo {
                base: base.clone(),
                effort: effort.clone(),
            },
        );
        if let Some(effort) = effort {
            raw_efforts_by_base
                .entry(base.clone())
                .or_default()
                .insert(effort.clone());
            full_id_by_base_effort
                .entry(base)
                .or_default()
                .insert(normalize_effort_id(&effort), trimmed.to_string());
        }
    }

    let mut efforts_by_base = HashMap::new();
    for (base, efforts) in raw_efforts_by_base {
        let mut list = efforts.into_iter().collect::<Vec<_>>();
        order_effort_ids(&mut list);
        efforts_by_base.insert(base, list);
    }

    let mut full_ids = full_ids.into_iter().collect::<Vec<_>>();
    full_ids.sort();
    let mut base_ids = base_ids.into_iter().collect::<Vec<_>>();
    base_ids.sort();

    Some(ModelCatalog {
        full_ids,
        current_model_id,
        base_ids,
        efforts_by_base,
        full_id_by_base_effort,
        info_by_full_id,
    })
}

fn extract_model_entries_from_list(
    list: &[serde_json::Value],
    entries: &mut Vec<(String, Option<String>)>,
) {
    for item in list {
        if let Some(id) = item.as_str() {
            let id = id.trim();
            if !id.is_empty() {
                entries.push((id.to_string(), None));
            }
            continue;
        }
        let Some(obj) = item.as_object() else {
            continue;
        };
        let id = obj
            .get("modelId")
            .or_else(|| obj.get("id"))
            .or_else(|| obj.get("model_id"))
            .or_else(|| obj.get("model"))
            .or_else(|| obj.get("name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let name = obj
            .get("name")
            .or_else(|| obj.get("display_name"))
            .or_else(|| obj.get("label"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        entries.push((id, name));
    }
}

fn extract_model_entries_from_map(
    map: &serde_json::Map<String, serde_json::Value>,
    entries: &mut Vec<(String, Option<String>)>,
) {
    for (key, value) in map {
        let display = value
            .as_object()
            .and_then(|nested| {
                nested
                    .get("name")
                    .or_else(|| nested.get("display_name"))
                    .or_else(|| nested.get("label"))
            })
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        entries.push((key.clone(), display));
    }
}

fn extract_model_entries(models: &serde_json::Value) -> Vec<(String, Option<String>)> {
    let mut entries = Vec::new();
    if let Some(array) = models.as_array() {
        extract_model_entries_from_list(array, &mut entries);
        return entries;
    }
    let Some(obj) = models.as_object() else {
        return entries;
    };
    if let Some(candidate) = obj
        .get("availableModels")
        .or_else(|| obj.get("available_models"))
        .or_else(|| obj.get("models"))
    {
        if let Some(array) = candidate.as_array() {
            extract_model_entries_from_list(array, &mut entries);
            return entries;
        }
        if let Some(map) = candidate.as_object() {
            extract_model_entries_from_map(map, &mut entries);
            return entries;
        }
    }
    extract_model_entries_from_map(obj, &mut entries);
    entries
}

pub fn compose_model_id(model_id: &str, reasoning_effort: Option<&str>) -> String {
    let base = model_id.trim();
    if base.is_empty() {
        return String::new();
    }
    match reasoning_effort
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(effort) => format!("{base}/{effort}"),
        None => base.to_string(),
    }
}

fn pick_default_effort(efforts: &[String]) -> Option<String> {
    let medium = efforts
        .iter()
        .find(|e| normalize_effort_id(e) == DEFAULT_REASONING_EFFORT)
        .cloned();
    medium.or_else(|| efforts.first().cloned())
}

fn build_resolved_model(model_id: String, reasoning_effort: Option<String>) -> ResolvedModel {
    let model_id = model_id.trim().to_string();
    let reasoning_effort = reasoning_effort
        .map(|value| normalize_effort_id(&value))
        .filter(|value| !value.is_empty());
    let full_model_id = compose_model_id(&model_id, reasoning_effort.as_deref());
    ResolvedModel {
        model_id,
        reasoning_effort,
        full_model_id,
    }
}

fn resolved_from_full_model_id(
    catalog: Option<&ModelCatalog>,
    full_model_id: &str,
) -> ResolvedModel {
    let trimmed = full_model_id.trim();
    if let Some(catalog) = catalog {
        if let Some(info) = catalog.info_by_full_id.get(trimmed) {
            return build_resolved_model(info.base.clone(), info.effort.clone());
        }
    }
    let (base, suffix) = split_model_id(trimmed);
    let reasoning_effort = suffix.filter(|value| is_known_effort_id(value));
    if reasoning_effort.is_some() {
        return build_resolved_model(base, reasoning_effort);
    }
    build_resolved_model(trimmed.to_string(), None)
}

pub fn resolve_model_id(
    requested_model: Option<&str>,
    requested_effort: Option<&str>,
    fallback_model: Option<&str>,
    catalog: Option<&ModelCatalog>,
) -> Result<ResolvedModel, String> {
    let model = requested_model
        .or(fallback_model)
        .unwrap_or("")
        .trim()
        .to_string();
    if model.is_empty() {
        return Err("model is required".to_string());
    }
    let effort_input = requested_effort
        .map(|e| e.trim())
        .filter(|e| !e.is_empty())
        .map(|e| e.to_string());

    if let Some(catalog) = catalog {
        let model_known = catalog.full_ids.contains(&model) || catalog.base_ids.contains(&model);
        if !model_known && requested_model.is_some() {
            if let Some(req_effort) = effort_input {
                let (_, suffix) = split_model_id(&model);
                if suffix.is_none() {
                    return Ok(build_resolved_model(model, Some(req_effort)));
                }
            }
            return Ok(resolved_from_full_model_id(None, &model));
        }

        let info = catalog.info_by_full_id.get(&model);
        let base = info
            .map(|i| i.base.clone())
            .unwrap_or_else(|| model.clone());
        let existing_effort = info.and_then(|i| i.effort.clone());
        let available_efforts = catalog
            .efforts_by_base
            .get(&base)
            .cloned()
            .unwrap_or_default();
        let supports_default_effort = available_efforts.len() >= 2;
        let effort_map = catalog.full_id_by_base_effort.get(&base);

        if let Some(req_effort) = effort_input {
            let req_norm = normalize_effort_id(&req_effort);
            if let Some(existing) = existing_effort.as_ref() {
                if normalize_effort_id(existing) != req_norm {
                    return Err(format!(
                        "model '{model}' already includes effort '{existing}'; requested '{req_effort}'"
                    ));
                }
                return Ok(resolved_from_full_model_id(Some(catalog), &model));
            }
            if let Some(map) = effort_map {
                if let Some(full_id) = map.get(&req_norm) {
                    return Ok(resolved_from_full_model_id(Some(catalog), full_id));
                }
            }
            let efforts = if available_efforts.is_empty() {
                effort_map
                    .map(|map| map.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default()
            } else {
                available_efforts.clone()
            };
            if efforts.is_empty() {
                return Err(format!("model '{base}' does not support reasoning_effort"));
            }
            return Err(format!(
                "invalid reasoning_effort '{req_effort}' for model '{base}'; available: {}",
                efforts.join(", ")
            ));
        }

        if existing_effort.is_some() {
            return Ok(resolved_from_full_model_id(Some(catalog), &model));
        }

        if supports_default_effort {
            if let Some(default_effort) = pick_default_effort(&available_efforts) {
                let default_norm = normalize_effort_id(&default_effort);
                if let Some(map) = effort_map {
                    if let Some(full_id) = map.get(&default_norm) {
                        return Ok(resolved_from_full_model_id(Some(catalog), full_id));
                    }
                }
            }
        } else if available_efforts.len() == 1 {
            let default_effort = available_efforts[0].clone();
            let default_norm = normalize_effort_id(&default_effort);
            if let Some(map) = effort_map {
                if let Some(full_id) = map.get(&default_norm) {
                    return Ok(resolved_from_full_model_id(Some(catalog), full_id));
                }
            }
        }

        return Ok(resolved_from_full_model_id(Some(catalog), &model));
    }

    if let Some(req_effort) = effort_input {
        let (_, suffix) = split_model_id(&model);
        if suffix.is_none() {
            return Ok(build_resolved_model(model, Some(req_effort)));
        }
    }

    Ok(resolved_from_full_model_id(None, &model))
}

#[cfg(test)]
mod tests {
    use super::{resolve_model_id, ModelCatalog};
    use std::collections::HashMap;

    #[test]
    fn model_catalog_default_model_id_prefers_current_model_id() {
        let catalog = ModelCatalog {
            full_ids: vec!["gemini-2.5-pro".to_string(), "auto-gemini-3".to_string()],
            current_model_id: Some("auto-gemini-3".to_string()),
            base_ids: Vec::new(),
            efforts_by_base: HashMap::new(),
            full_id_by_base_effort: HashMap::new(),
            info_by_full_id: HashMap::new(),
        };

        assert_eq!(catalog.default_model_id(), Some("auto-gemini-3"));
    }

    #[test]
    fn model_catalog_extracts_model_id_from_available_models_model_id_field() {
        let catalog = super::build_model_catalog(&serde_json::json!({
            "availableModels": [
                {
                    "modelId": "gpt-5.2-codex/high",
                    "name": "gpt-5.2-codex (high)"
                }
            ]
        }))
        .expect("catalog");

        assert!(catalog
            .full_ids()
            .iter()
            .any(|id| id == "gpt-5.2-codex/high"));
    }

    #[test]
    fn resolve_model_id_allows_explicit_unknown_model_when_catalog_is_present() {
        let catalog = ModelCatalog {
            full_ids: vec!["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()],
            current_model_id: Some("gemini-2.5-pro".to_string()),
            base_ids: vec!["gemini-2.5-pro".to_string(), "gemini-2.5-flash".to_string()],
            efforts_by_base: HashMap::new(),
            full_id_by_base_effort: HashMap::new(),
            info_by_full_id: HashMap::new(),
        };

        let resolved = resolve_model_id(Some("gemini-3-pro-exp"), None, None, Some(&catalog))
            .expect("explicit unknown model should be accepted");

        assert_eq!(resolved.model_id, "gemini-3-pro-exp");
        assert_eq!(resolved.reasoning_effort, None);
        assert_eq!(resolved.full_model_id, "gemini-3-pro-exp");
    }

    #[test]
    fn resolve_model_id_allows_explicit_unknown_model_with_reasoning_effort() {
        let catalog = ModelCatalog {
            full_ids: vec!["gpt-5/medium".to_string(), "gpt-5/high".to_string()],
            current_model_id: Some("gpt-5/medium".to_string()),
            base_ids: vec!["gpt-5".to_string()],
            efforts_by_base: HashMap::from([(
                "gpt-5".to_string(),
                vec!["medium".to_string(), "high".to_string()],
            )]),
            full_id_by_base_effort: HashMap::from([(
                "gpt-5".to_string(),
                HashMap::from([
                    ("medium".to_string(), "gpt-5/medium".to_string()),
                    ("high".to_string(), "gpt-5/high".to_string()),
                ]),
            )]),
            info_by_full_id: HashMap::new(),
        };

        let resolved = resolve_model_id(Some("gpt-6"), Some("xhigh"), None, Some(&catalog))
            .expect("explicit unknown model with effort should be accepted");

        assert_eq!(resolved.model_id, "gpt-6");
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(resolved.full_model_id, "gpt-6/xhigh");
    }
}
