use super::*;

impl RobotModel {
    pub(super) fn validate(&self, source: &str) -> Result<()> {
        let text = |s: &str| !s.is_empty() && s.len() <= 4096 && !s.contains('\0');
        ensure!(
            self.schema_version == 1
                && self.robot_version.len() <= 64
                && self.failure.as_ref().is_none_or(|s| text(s))
                && self.languages.len() <= 64
                && self.languages.iter().all(|s| text(s) && s.len() <= 64)
                && self.definitions.len()
                    + self.imports.len()
                    + self.calls.len()
                    + self.diagnostics.len()
                    <= 20_000,
            "invalid Robot parser protocol or extraction limit"
        );
        ensure!(
            self.failure.is_some()
                || self.robot_version == "7.5"
                || self
                    .robot_version
                    .strip_prefix("7.5.")
                    .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())),
            "unsupported Robot parser protocol version"
        );
        for (id, definition) in self.definitions.iter().enumerate() {
            ensure!(
                definition.id == id && text(&definition.name) && definition.span.valid(source),
                "invalid Robot definition or source span"
            );
        }
        for import in &self.imports {
            ensure!(
                text(&import.name)
                    && import.alias.as_ref().is_none_or(|s| text(s))
                    && import.span.valid(source),
                "invalid Robot import or source span"
            );
        }
        for call in &self.calls {
            ensure!(
                text(&call.name)
                    && call.span.valid(source)
                    && (1..=2).contains(&call.alternatives.len())
                    && call.alternatives[0] == call.name
                    && call.alternatives.iter().all(|s| text(s))
                    && call.owner.is_none_or(|id| id < self.definitions.len())
                    && call.target.is_none_or(|id| !call.ambiguous
                        && self
                            .definitions
                            .get(id)
                            .is_some_and(|d| d.kind == RobotDefinitionKind::Keyword)),
                "invalid Robot call, owner or target"
            );
        }
        for issue in &self.diagnostics {
            ensure!(
                matches!(issue.code.as_str(), "syntax" | "embedded_syntax")
                    && issue.span.valid(source),
                "invalid Robot diagnostic or source span"
            );
        }
        Ok(())
    }
}
