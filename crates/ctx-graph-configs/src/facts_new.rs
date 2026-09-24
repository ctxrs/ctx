use super::*;

impl Facts {
    pub(super) fn new(path: &str, hash: &str) -> Self {
        Self(FileFacts {
            path: path.into(),
            hash: hash.into(),
            module: path.into(),
            nodes: vec![],
            edges: vec![],
            references: vec![],
            diagnostics: vec![],
        })
    }
    pub(super) fn node(
        &mut self,
        label: &str,
        kind: &str,
        key: Option<String>,
        line: u32,
        metadata: Value,
    ) -> String {
        let id = format!("config:{}:{}", self.0.path, self.0.nodes.len());
        self.0.nodes.push(Node {
            id: id.clone(),
            label: label.into(),
            kind: kind.into(),
            file: self.0.path.clone(),
            line: Some(line),
            end_line: Some(line),
            qualified_name: Some(label.into()),
            binding_key: key,
            metadata,
        });
        id
    }
    pub(super) fn root(&mut self, language: &str, metadata: Value) -> String {
        let path = self.0.path.clone();
        let mut data = metadata;
        data["language"] = json!(language);
        self.node(
            basename(&path),
            "module",
            Some(format!("config:file:{path}")),
            1,
            data,
        )
    }
    pub(super) fn edge(&mut self, from: &str, to: &str, relation: &str, line: u32) {
        if from == to
            || self
                .0
                .edges
                .iter()
                .any(|e| e.source == from && e.target == to && e.relation == relation)
        {
            return;
        }
        self.0.edges.push(Edge {
            id: format!("config-edge:{}:{}", self.0.path, self.0.edges.len()),
            source: from.into(),
            target: to.into(),
            relation: relation.into(),
            directed: true,
            file: Some(self.0.path.clone()),
            line: Some(line),
            confidence: "static".into(),
            metadata: Value::Null,
        });
    }
    pub(super) fn reference(
        &mut self,
        from: &str,
        label: &str,
        relation: &str,
        keys: Vec<String>,
        line: u32,
    ) {
        if self.0.references.iter().any(|r| {
            r.source == from
                && r.label == label
                && r.relation == relation
                && r.candidate_keys == keys
        }) {
            return;
        }
        self.0.references.push(Reference {
            id: format!("config-ref:{}:{}", self.0.path, self.0.references.len()),
            source: from.into(),
            label: label.into(),
            relation: relation.into(),
            file: self.0.path.clone(),
            line,
            candidate_keys: keys,
            reason: "Declared target is external, unavailable, or ambiguous".into(),
        });
    }
    pub(super) fn package(
        &mut self,
        root: &str,
        ecosystem: &str,
        name: &str,
        version: Option<&str>,
    ) -> String {
        let id = self.node(
            name,
            "package",
            Some(package_key(ecosystem, name)),
            1,
            json!({"ecosystem":ecosystem,"version":version}),
        );
        self.edge(root, &id, "contains", 1);
        id
    }
    pub(super) fn dependency(&mut self, from: &str, ecosystem: &str, name: &str, line: u32) {
        let key = package_key(ecosystem, name);
        if name.is_empty()
            || self
                .0
                .nodes
                .iter()
                .any(|n| n.id == from && n.binding_key.as_ref() == Some(&key))
        {
            return;
        }
        self.reference(from, name, "depends_on", vec![key], line);
    }
}
