mod agents;
mod install;
mod paths;
mod selection;
mod target;

pub use agents::{agent_from_name, parse_skill_agent, picker_agents, SkillAgentArg};
pub use install::{
    default_maintenance_selection, execute_install, execute_status, install_target, status_target,
    InstallResult, SkillInstallReceipt, SkillInstallRequest, SkillInstallStatus, SkillMetadata,
    SkillStatusReceipt, SkillStatusRequest, StatusResult,
};
pub use paths::{bundled_hash, ensure_path_inside, sanitize_skill_name, sha256_hex, PathContext};
pub use selection::{
    default_agent_selection, default_noninteractive_agents, detected_agents,
    explicit_agent_selection, parse_picker_selection, picker_agent_selection, SkillAgentSelection,
    SkillSelectionSource,
};
pub use target::{resolve_targets_for_agents, single_target, SkillScope, SkillTarget};

pub const BUNDLED_SKILL_NAME: &str = "ctx";
pub const BUNDLED_SKILL_BODY: &str = include_str!("../../../../skills/ctx/SKILL.md");
const LEGACY_BUNDLED_SKILL_NAME: &str = "ctx-agent-history-search";
const METADATA_FILE: &str = ".ctx-skill.json";
const LEGACY_BUNDLED_SKILL_HASHES: &[&str] = &[
    "sha256:d76cd55f506f6d8605f2fed933a16e4ab995b3a4ab8e6d96bfd84d469872b3d6",
    "sha256:e0c0088162ed194d4961d856d441c3f46387609be41c5df625d546ddd4550946",
    "sha256:9c2ddb5ed64da0471050af225addd5823ef7fc2b9bbcea27e72a3c8553234774",
    "sha256:b4210c5e3c4fd8a8e62335ca61879bb88d026c092b4b663a9ae3ad15f34ee2ba",
    "sha256:59623e2cabd7857a518da19f995ca86e65fe67e6337fa334a0c86bef78891c6f",
    "sha256:287e5470664e6225114c6676d56e6f98eb6f83f3ebe7bac980532c6dabbee0c6",
    "sha256:64e3cf9c676e5edfdb1a825b27abc1971e5959577c15709934421def71405ae2",
    "sha256:c72dbfae7d0af06c18d119f586e22e6cd3ba9444cc6a01e7d4662f2cf98d86d8",
    "sha256:87f435ad67bc5afdc4120f1ca9090aa6c2b71ee87c0bdeeb7e0bde33778c32ed",
    "sha256:3da0ddcff0409cc9d5912cf2019fdaf00d4faa84f000fb76041b670f94aa2986",
    "sha256:b606132c882a0ce0db2c049c599cfdb7db113d2a6690a58a6c329b5101c752c9",
    "sha256:c0647d2368714b09a5f652583b9f2c34e88502b0ab441ba44c4698313675dbcc",
];
