use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Map, Value};

pub(super) async fn translate_prompt_items_for_app_server(items: Vec<Value>) -> Result<Vec<Value>> {
    let mut translated = Vec::with_capacity(items.len());
    for item in items {
        translated.push(translate_prompt_item_for_app_server(item).await?);
    }
    Ok(translated)
}

async fn translate_prompt_item_for_app_server(item: Value) -> Result<Value> {
    let Some(obj) = item.as_object() else {
        anyhow::bail!("Codex prompt items must be JSON objects");
    };
    let Some(item_type) = obj.get("type").and_then(Value::as_str) else {
        anyhow::bail!("Codex prompt items must include a string `type` field");
    };

    match item_type {
        "text" => {
            let text = obj
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("text prompt item missing `text`"))?;
            let mut translated = Map::new();
            translated.insert("type".to_string(), Value::String("text".to_string()));
            translated.insert("text".to_string(), Value::String(text.to_string()));
            if let Some(text_elements) =
                obj.get("textElements").or_else(|| obj.get("text_elements"))
            {
                translated.insert("textElements".to_string(), text_elements.clone());
            }
            Ok(Value::Object(translated))
        }
        "image" => Ok(json!({
            "type": "image",
            "url": translate_image_item_to_url(obj)?,
        })),
        "image_ref" => {
            let blob_id = obj
                .get("blob_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("image_ref prompt item missing `blob_id`"))?;
            let data_root = codex_runtime_data_root().ok_or_else(|| {
                anyhow::anyhow!(
                    "image_ref prompt item requires CTX_DATA_ROOT or CTX_DATA_ROOT_HOST"
                )
            })?;
            let blob_path = data_root.join("blobs").join(blob_id);
            Ok(json!({
                "type": "localImage",
                "path": blob_path.to_string_lossy().to_string(),
            }))
        }
        "local_image" => {
            let path = obj
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("local_image prompt item missing `path`"))?;
            Ok(json!({
                "type": "localImage",
                "path": path,
            }))
        }
        "localImage" | "skill" | "mention" => Ok(item),
        other => anyhow::bail!("unsupported Codex prompt item type `{other}`"),
    }
}

fn translate_image_item_to_url(obj: &serde_json::Map<String, Value>) -> Result<String> {
    if let Some(url) = obj.get("url").and_then(Value::as_str) {
        return Ok(url.to_string());
    }

    let data = obj
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("image prompt item missing `data` or `url`"))?;
    if data
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
    {
        return Ok(data.to_string());
    }

    let mime_type = obj
        .get("mime_type")
        .or_else(|| obj.get("mimeType"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("image prompt item missing `mime_type`/`mimeType`"))?;
    Ok(format!("data:{mime_type};base64,{data}"))
}

fn codex_runtime_data_root() -> Option<PathBuf> {
    std::env::var("CTX_DATA_ROOT")
        .ok()
        .or_else(|| std::env::var("CTX_DATA_ROOT_HOST").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
