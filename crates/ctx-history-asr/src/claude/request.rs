use crate::{
    records::{copy, meta, Writer},
    spool::Spool,
    Provider, Result,
};
use serde_json::{json, Value};

pub(super) fn record(spool: &Spool, writer: &mut Writer, v: &Value) -> Result<()> {
    match v["type"].as_str() {
        Some("api-request-blob") => {
            if let Some(hash) = v["hash"].as_str() {
                let mut file = tempfile::NamedTempFile::new_in(&spool.bundle)?;
                serde_json::to_writer(file.as_file_mut(), &v["message"])?;
                let blob = spool.blob(file)?;
                spool.save(&format!("request-blob:{hash}"), &blob)?;
                writer.emit(json!({"type":"com.anthropic.claude-code/api-request-blob","hash":hash,"blob":blob}),v)?;
            }
        }
        Some("api-request") => {
            if let Some(id) = v["id"].as_str() {
                spool.save(&format!("request:{id}"), v)?;
            }
            writer.vendor(v, "api-request")?;
        }
        _ => writer.vendor(v, "api-request-shape")?,
    }
    Ok(())
}
pub(super) fn inference(spool: &Spool, r: &mut Value, v: &Value, response: &str) -> Result<()> {
    let Some(request) = v["requestRef"].as_str() else {
        return Ok(());
    };
    let Some(req) = spool.entity(&format!("request:{request}"))? else {
        return Ok(());
    };
    copy(r, "params", &req, "params");
    let mut details = json!({});
    for key in [
        "querySource",
        "shapeHash",
        "cacheControl",
        "withheld",
        "cut",
    ] {
        copy(&mut details, key, &req, key);
    }
    meta(r, Provider::Claude, "apiRequest", &details);
    let mut context = json!({"entries":[]});
    let mut entries = Vec::new();
    let mut count = 0u64;
    if let Some(base) = req["base"].as_str() {
        let prior = spool.entity(&format!("request-response:{base}"))?;
        if let (Some(prior), Some(keep)) = (prior, req["keep"].as_u64()) {
            if prior["count"].as_u64().is_some_and(|n| keep <= n) {
                context["prefixFrom"] = json!({"inferenceId":prior["response"],"count":keep});
                count = keep;
            } else {
                meta(
                    r,
                    Provider::Claude,
                    "unresolvedRequestPrefix",
                    &json!({"base":base,"keep":keep}),
                );
            }
        } else {
            meta(
                r,
                Provider::Claude,
                "unresolvedRequestPrefix",
                &json!({"base":base,"keep":req["keep"]}),
            );
        }
    }
    for hash in req["tail"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some(blob) = spool.entity(&format!("request-blob:{hash}"))? {
            entries.push(json!({"blob":blob}));
            count += 1;
        } else {
            meta(r, Provider::Claude, "missingRequestBlob", &json!(hash));
        }
    }
    // Digests in this native format are not all SHA-256 request-byte digests.
    if let Some(digest) = req.get("requestDigest") {
        meta(r, Provider::Claude, "requestDigest", digest);
    }
    context["entries"] = json!(entries);
    r["context"] = context;
    spool.save(
        &format!("request-response:{request}"),
        &json!({"response":response,"count":count}),
    )?;
    Ok(())
}
