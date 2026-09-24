use super::*;

impl PythonContext {
    pub(super) fn module_member(module: &str, name: &str) -> String {
        format!(
            "{}:{module}:{name}",
            if name.contains('.') {
                "python-member"
            } else {
                "python"
            }
        )
    }
}
