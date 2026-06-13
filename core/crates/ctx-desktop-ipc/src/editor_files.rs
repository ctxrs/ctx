use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DesktopEditorTarget {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "vscode")]
    VsCode,
    #[serde(rename = "vscode_insiders")]
    VsCodeInsiders,
    #[serde(rename = "cursor")]
    Cursor,
    #[serde(rename = "windsurf")]
    Windsurf,
    #[serde(rename = "antigravity")]
    Antigravity,
    #[serde(rename = "idea")]
    Idea,
    #[serde(rename = "pycharm")]
    Pycharm,
    #[serde(rename = "xcode")]
    Xcode,
    #[serde(rename = "android_studio")]
    AndroidStudio,
    #[serde(rename = "custom")]
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DesktopEditorSettings {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub custom_command: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub remote_authority: Option<String>,
    pub target: DesktopEditorTarget,
}

impl Default for DesktopEditorSettings {
    fn default() -> Self {
        Self {
            custom_command: None,
            remote_authority: None,
            target: DesktopEditorTarget::System,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopOpenFileReq {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub col: Option<u32>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub line: Option<u32>,
    pub path: String,
    pub worktree_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopOpenPathReq {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub col: Option<u32>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub line: Option<u32>,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopReadBinaryFileResp {
    pub bytes: Vec<u8>,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopSaveTextFileReq {
    pub contents: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub suggested_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopRestartLocalDaemonReq {
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopUploadBlobReq {
    pub bytes: Vec<u8>,
    pub mime_type: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopCodexLoginRelayReq {
    pub callback_url: String,
    pub completion_token: String,
    pub login_id: String,
}
