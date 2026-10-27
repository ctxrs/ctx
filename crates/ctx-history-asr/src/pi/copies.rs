//! Fork entries retain their IDs. A native parent path is resolved by its header.
use super::*;
pub(crate) fn prepare(spool: &Spool, header: &Value) -> Result<()> {
    let Some(parent) = header["_parentId"].as_str() else {
        return Ok(());
    };
    let Some(file) = header["_parentPath"]
        .as_str()
        .or_else(|| header["parentSession"].as_str())
    else {
        return Ok(());
    };
    let path = std::path::Path::new(file);
    if !path.is_file() {
        return Ok(());
    }
    let mut reader = crate::source::reader(path)?;
    let mut bytes = Vec::new();
    while let Some((_, oversized)) = crate::source::line(&mut *reader, &mut bytes, spool.limit)? {
        if oversized {
            continue;
        }
        let Some(v) = crate::source::decode(&bytes) else {
            continue;
        };
        if v["type"] == "session" {
            continue;
        }
        if let Some(id) = v["id"].as_str() {
            spool.save(
                &format!("pi:copy:{id}"),
                &json!({"session":crate::records::session_ref(P,parent),"proof":"native_id"}),
            )?;
        }
    }
    Ok(())
}
pub(crate) fn copy(spool: &Spool, v: &Value) -> Result<Option<Value>> {
    let Some(id) = v["id"].as_str().filter(|_| v["type"] != "session") else {
        return Ok(None);
    };
    spool.entity(&format!("pi:copy:{id}"))
}
