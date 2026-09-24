use super::*;

impl<'a> Inventory<'a> {
    pub(super) fn new(root: &'a Path, paths: &[String]) -> Self {
        Self {
            root,
            files: paths.iter().cloned().collect(),
            configs: BTreeMap::new(),
        }
    }
    pub(super) fn read_bytes(&self, relative: &str, limit: u64) -> Result<Option<Vec<u8>>> {
        ensure!(
            join("", relative).as_deref() == Some(relative),
            "configuration path must stay inside the repository"
        );
        let mut path = self.root.to_path_buf();
        for component in relative.split('/') {
            path.push(component);
            match std::fs::symlink_metadata(&path) {
                Ok(meta) => ensure!(
                    !meta.file_type().is_symlink(),
                    "configuration paths must not traverse symlinks"
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e).context("cannot inspect project configuration"),
            }
        }
        let bytes = crate::index::read_source(&path, limit)?
            .1
            .context("project configuration exceeds its size limit")?;
        Ok(Some(bytes))
    }
    pub(super) fn read(&self, relative: &str, limit: u64) -> Result<Option<String>> {
        self.read_bytes(relative, limit)?
            .map(String::from_utf8)
            .transpose()
            .context("project configuration is not UTF-8")
    }
    pub(super) fn config(&mut self, relative: &str) -> Result<Option<String>> {
        if let Some(value) = self.configs.get(relative) {
            return Ok(value.clone());
        }
        let value = self.read(relative, 1024 * 1024)?;
        self.configs.insert(relative.into(), value.clone());
        Ok(value)
    }
    pub(super) fn config_fingerprint(&self, extension: &str) -> String {
        let mut items = vec![extension];
        for (path, value) in &self.configs {
            items.push(path);
            items.push(value.as_deref().unwrap_or("<missing>"));
        }
        digest(items)
    }
}
