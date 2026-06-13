mod app_update;
mod connection;
mod daemon;
mod declarations;
mod editor_files;
mod external_links;
mod remote_workspace;
mod storage;
mod webview;
mod window_notifications;

pub use app_update::*;
pub use connection::*;
pub use daemon::*;
pub use declarations::*;
pub use editor_files::*;
pub use external_links::*;
pub use remote_workspace::*;
pub use storage::*;
pub use webview::*;
pub use window_notifications::*;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::typescript_declarations;

    #[test]
    fn generated_typescript_is_current() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let output_path = manifest_dir.join("../../apps/web/src/generated/desktop-ipc.ts");
        let existing = fs::read_to_string(&output_path)
            .unwrap_or_else(|err| panic!("reading {}: {err}", output_path.display()));
        assert_eq!(
            existing,
            typescript_declarations(),
            "desktop IPC TypeScript bindings are stale: regenerate {}",
            output_path.display()
        );
    }
}
