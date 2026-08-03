//! Filesystem and helper-launch contract shared by the OSS host and Pro helper.
//!
//! These names are part of the exact local Protocol V1 integration contract.
//! Callers remain responsible for path authorization and filesystem safety.

use std::path::{Path, PathBuf};

pub const CTX_PRO_DATA_ROOT_ENV: &str = "CTX_PRO_DATA_ROOT";
pub const CTX_PRO_INSTALLATION_ID_ENV: &str = "CTX_PRO_INSTALLATION_ID";
pub const PRO_INSTALLATION_ID_FILE_NAME: &str = "install.json";
pub const PRO_ROOT_DIRECTORY_NAME: &str = "pro";
pub const PRO_BIN_DIRECTORY_NAME: &str = "bin";
pub const PRO_DOWNLOADS_DIRECTORY_NAME: &str = "downloads";
pub const PRO_GRAPH_DIRECTORY_NAME: &str = "graph";
pub const PRO_LIFECYCLE_LOCK_FILE_NAME: &str = ".ctx-pro.lifecycle.lock";
pub const PRO_PRESERVED_DATA_MARKER_FILE_NAME: &str = ".ctx-pro.data-preserved";

#[cfg(windows)]
pub const PRO_HELPER_FILE_NAME: &str = "ctx-pro.exe";
#[cfg(not(windows))]
pub const PRO_HELPER_FILE_NAME: &str = "ctx-pro";

pub const PRO_PREVIOUS_HELPER_FILE_NAME: &str = "ctx-pro.previous";
pub const PRO_PREVIOUS_MARKER_FILE_NAME: &str = "ctx-pro.previous.install.json";
pub const PRO_TRANSACTION_JOURNAL_FILE_NAME: &str = ".ctx-pro.transaction.json";
pub const PRO_TRANSACTION_JOURNAL_NEXT_FILE_NAME: &str = ".ctx-pro.transaction.json.next";
pub const PRO_TRANSACTION_HELPER_FILE_NAME: &str = ".ctx-pro.transaction.helper";
pub const PRO_TRANSACTION_MARKER_FILE_NAME: &str = ".ctx-pro.transaction.marker";
pub const PRO_PUBLISH_HELPER_FILE_NAME: &str = ".ctx-pro.publish.helper";
pub const PRO_PUBLISH_MARKER_FILE_NAME: &str = ".ctx-pro.publish.marker";
pub const PRO_ROLLBACK_HELPER_FILE_NAME: &str = ".ctx-pro.rollback.helper";
pub const PRO_ROLLBACK_MARKER_FILE_NAME: &str = ".ctx-pro.rollback.marker";
pub const PRO_GRAPH_RECORD_ID_DOMAIN: &str = "ctx-pro-installation-graph-v1";
pub const PRO_CLOCK_RECORD_ID_DOMAIN: &str = "ctx-pro-entitlement-clock-v1";

const PRO_GRAPH_CONTROL_FILES: [&[u8]; 4] = [
    b"graph-manifest.ctxm",
    b".graph-materializer-control.ctxc",
    b"graph-manifest.publication-lock",
    b"graph-materializer.lock",
];

/// Returns whether `name` is one exact current Flat/FST graph artifact name.
///
/// This is the public lifecycle inventory contract. Prefixes and extensions
/// that merely resemble a current artifact intentionally fail closed.
#[must_use]
pub fn is_pro_graph_artifact_file_name(name: &[u8]) -> bool {
    PRO_GRAPH_CONTROL_FILES.contains(&name)
        || matches_hashed_name(name, b".graph-manifest-", &[64], b".candidate")
        || matches_hashed_name(name, b".graph-materializer-control-", &[64], b".candidate")
        || is_manifest_scratch_file_name(name)
        || is_materializer_scratch_file_name(name)
        || matches_hashed_name(name, b"graph-segment-", &[64, 8], b".ctxs")
        || matches_hashed_name(name, b".graph-materializer-journal-", &[64, 64], b".ctxj")
        || matches_hashed_name(name, b".graph-materializer-pack-", &[64, 64], b".ctxp")
}

fn is_manifest_scratch_file_name(name: &[u8]) -> bool {
    let Some(body) = name
        .strip_prefix(b".graph-manifest-open-")
        .and_then(|body| body.strip_suffix(b".encrypted-tmp"))
    else {
        return false;
    };
    let Some(separator) = body.iter().position(|byte| *byte == b'-') else {
        return false;
    };
    let (pid, sequence_with_separator) = body.split_at(separator);
    let sequence = &sequence_with_separator[1..];
    is_canonical_decimal(pid)
        && pid != b"0"
        && decimal_fits(pid, u32::MAX as u64)
        && is_canonical_decimal(sequence)
        && decimal_fits(sequence, u64::MAX)
}

fn is_materializer_scratch_file_name(name: &[u8]) -> bool {
    let Some(body) = name
        .strip_prefix(b".graph-materializer-open-")
        .and_then(|body| body.strip_suffix(b".encrypted-tmp"))
    else {
        return false;
    };
    let Some((object, sequence_with_separator)) = body.split_at_checked(64) else {
        return false;
    };
    let Some(sequence) = sequence_with_separator.strip_prefix(b"-") else {
        return false;
    };
    object.iter().copied().all(is_lower_hex)
        && is_canonical_decimal(sequence)
        && decimal_fits(sequence, u64::MAX)
}

fn is_canonical_decimal(value: &[u8]) -> bool {
    !value.is_empty()
        && (value.len() == 1 || value[0] != b'0')
        && value.iter().all(u8::is_ascii_digit)
}

fn decimal_fits(value: &[u8], maximum: u64) -> bool {
    value
        .iter()
        .try_fold(0_u64, |parsed, byte| {
            parsed
                .checked_mul(10)?
                .checked_add(u64::from(*byte - b'0'))
                .filter(|parsed| *parsed <= maximum)
        })
        .is_some()
}

fn matches_hashed_name(
    name: &[u8],
    prefix: &[u8],
    hex_field_bytes: &[usize],
    suffix: &[u8],
) -> bool {
    let Some(mut remaining) = name
        .strip_prefix(prefix)
        .and_then(|body| body.strip_suffix(suffix))
    else {
        return false;
    };
    for (index, field_bytes) in hex_field_bytes.iter().copied().enumerate() {
        let Some((field, tail)) = remaining.split_at_checked(field_bytes) else {
            return false;
        };
        if !field.iter().copied().all(is_lower_hex) {
            return false;
        }
        remaining = tail;
        if index + 1 < hex_field_bytes.len() {
            let Some(tail) = remaining.strip_prefix(b"-") else {
                return false;
            };
            remaining = tail;
        }
    }
    remaining.is_empty()
}

const fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (byte >= b'a' && byte <= b'f')
}

/// Accepts only the canonical lower-case hyphenated representation of a
/// non-nil UUID. This keeps the root identity opaque and its derived native
/// credential record names unambiguous across every host platform.
#[must_use]
pub fn valid_pro_installation_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .is_ok_and(|parsed| !parsed.is_nil() && parsed.hyphenated().to_string() == value)
}

#[must_use]
pub fn pro_graph_record_id(
    installation_id: &str,
    installation_key_thumbprint: &str,
) -> Option<String> {
    pro_record_id(
        PRO_GRAPH_RECORD_ID_DOMAIN,
        installation_id,
        installation_key_thumbprint,
    )
}

#[must_use]
pub fn pro_clock_record_id(
    installation_id: &str,
    installation_key_thumbprint: &str,
) -> Option<String> {
    pro_record_id(
        PRO_CLOCK_RECORD_ID_DOMAIN,
        installation_id,
        installation_key_thumbprint,
    )
}

fn pro_record_id(
    domain: &str,
    installation_id: &str,
    installation_key_thumbprint: &str,
) -> Option<String> {
    if !valid_pro_installation_id(installation_id)
        || installation_key_thumbprint.is_empty()
        || installation_key_thumbprint.len() > 256
    {
        return None;
    }
    Some(format!(
        "{domain}:{installation_id}:{installation_key_thumbprint}"
    ))
}

#[derive(Debug, Clone, Copy)]
pub struct ProFilesystemLayout<'a> {
    data_root: &'a Path,
}

impl<'a> ProFilesystemLayout<'a> {
    #[must_use]
    pub const fn new(data_root: &'a Path) -> Self {
        Self { data_root }
    }

    #[must_use]
    pub const fn data_root(self) -> &'a Path {
        self.data_root
    }

    #[must_use]
    pub fn installation_id_path(self) -> PathBuf {
        self.data_root.join(PRO_INSTALLATION_ID_FILE_NAME)
    }

    #[must_use]
    pub fn pro_root(self) -> PathBuf {
        self.data_root.join(PRO_ROOT_DIRECTORY_NAME)
    }

    #[must_use]
    pub fn bin_dir(self) -> PathBuf {
        self.pro_root().join(PRO_BIN_DIRECTORY_NAME)
    }

    #[must_use]
    pub fn downloads_dir(self) -> PathBuf {
        self.pro_root().join(PRO_DOWNLOADS_DIRECTORY_NAME)
    }

    #[must_use]
    pub fn graph_dir(self) -> PathBuf {
        self.pro_root().join(PRO_GRAPH_DIRECTORY_NAME)
    }

    #[must_use]
    pub fn lifecycle_lock_path(self) -> PathBuf {
        self.pro_root().join(PRO_LIFECYCLE_LOCK_FILE_NAME)
    }

    #[must_use]
    pub fn preserved_data_marker_path(self) -> PathBuf {
        self.pro_root().join(PRO_PRESERVED_DATA_MARKER_FILE_NAME)
    }

    #[must_use]
    pub fn helper_path(self) -> PathBuf {
        self.bin_dir().join(PRO_HELPER_FILE_NAME)
    }

    #[must_use]
    pub fn helper_marker_path(self) -> PathBuf {
        self.bin_dir()
            .join(format!("{PRO_HELPER_FILE_NAME}.install.json"))
    }

    #[must_use]
    pub fn previous_helper_path(self) -> PathBuf {
        self.bin_dir().join(PRO_PREVIOUS_HELPER_FILE_NAME)
    }

    #[must_use]
    pub fn previous_marker_path(self) -> PathBuf {
        self.bin_dir().join(PRO_PREVIOUS_MARKER_FILE_NAME)
    }

    #[must_use]
    pub fn transaction_journal_path(self) -> PathBuf {
        self.bin_dir().join(PRO_TRANSACTION_JOURNAL_FILE_NAME)
    }

    #[must_use]
    pub fn transaction_journal_next_path(self) -> PathBuf {
        self.bin_dir().join(PRO_TRANSACTION_JOURNAL_NEXT_FILE_NAME)
    }

    #[must_use]
    pub fn transaction_helper_path(self) -> PathBuf {
        self.bin_dir().join(PRO_TRANSACTION_HELPER_FILE_NAME)
    }

    #[must_use]
    pub fn transaction_marker_path(self) -> PathBuf {
        self.bin_dir().join(PRO_TRANSACTION_MARKER_FILE_NAME)
    }

    #[must_use]
    pub fn publish_helper_path(self) -> PathBuf {
        self.bin_dir().join(PRO_PUBLISH_HELPER_FILE_NAME)
    }

    #[must_use]
    pub fn publish_marker_path(self) -> PathBuf {
        self.bin_dir().join(PRO_PUBLISH_MARKER_FILE_NAME)
    }

    #[must_use]
    pub fn rollback_helper_path(self) -> PathBuf {
        self.bin_dir().join(PRO_ROLLBACK_HELPER_FILE_NAME)
    }

    #[must_use]
    pub fn rollback_marker_path(self) -> PathBuf {
        self.bin_dir().join(PRO_ROLLBACK_MARKER_FILE_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_has_one_exact_root_relative_shape() {
        let root = Path::new("/ctx-root");
        let layout = ProFilesystemLayout::new(root);
        assert_eq!(layout.installation_id_path(), root.join("install.json"));
        assert_eq!(layout.pro_root(), root.join("pro"));
        assert_eq!(layout.bin_dir(), root.join("pro/bin"));
        assert_eq!(layout.graph_dir(), root.join("pro/graph"));
        assert_eq!(
            layout.lifecycle_lock_path(),
            root.join("pro/.ctx-pro.lifecycle.lock")
        );
        assert_eq!(
            layout.preserved_data_marker_path(),
            root.join("pro/.ctx-pro.data-preserved")
        );
        let helper = layout.helper_path();
        let bin = layout.bin_dir();
        assert_eq!(helper.parent(), Some(bin.as_path()));
        assert_eq!(
            layout.transaction_journal_path(),
            root.join("pro/bin/.ctx-pro.transaction.json")
        );
    }

    #[test]
    fn flat_graph_inventory_accepts_only_current_exact_artifact_names() {
        let object = "a".repeat(64);
        let materialization = "b".repeat(64);
        let sequence = "b".repeat(8);
        for name in [
            "graph-manifest.ctxm".to_owned(),
            ".graph-materializer-control.ctxc".to_owned(),
            "graph-manifest.publication-lock".to_owned(),
            "graph-materializer.lock".to_owned(),
            format!(".graph-manifest-{object}.candidate"),
            format!(".graph-materializer-control-{object}.candidate"),
            ".graph-manifest-open-1-0.encrypted-tmp".to_owned(),
            format!(".graph-materializer-open-{object}-0.encrypted-tmp"),
            format!("graph-segment-{object}-{sequence}.ctxs"),
            format!(".graph-materializer-journal-{materialization}-{object}.ctxj"),
            format!(".graph-materializer-pack-{materialization}-{object}.ctxp"),
        ] {
            assert!(is_pro_graph_artifact_file_name(name.as_bytes()), "{name}");
        }

        for name in [
            "ctx-pro.db".to_owned(),
            "segments".to_owned(),
            ".graph-manifest-deadbeef.candidate".to_owned(),
            format!(".graph-manifest-{}.candidate", "A".repeat(64)),
            format!(".graph-manifest-{object}.candidate.extra"),
            ".graph-manifest-open-0-0.encrypted-tmp".to_owned(),
            ".graph-manifest-open-01-0.encrypted-tmp".to_owned(),
            format!(".graph-materializer-open-{object}-00.encrypted-tmp"),
            format!("graph-segment-{object}-{}.ctxs", "0".repeat(7)),
            format!(".graph-materializer-pack-{materialization}-{object}.ctxp.extra"),
        ] {
            assert!(!is_pro_graph_artifact_file_name(name.as_bytes()), "{name}");
        }
    }

    #[test]
    fn opaque_installation_identity_derives_stable_record_ids() {
        let id = "6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8";
        assert_eq!(
            pro_graph_record_id(id, "thumbprint").as_deref(),
            Some("ctx-pro-installation-graph-v1:6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8:thumbprint")
        );
        assert_eq!(
            pro_clock_record_id(id, "thumbprint").as_deref(),
            Some("ctx-pro-entitlement-clock-v1:6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8:thumbprint")
        );
        assert!(pro_graph_record_id("not-a-uuid", "thumbprint").is_none());
        assert!(pro_graph_record_id(&uuid::Uuid::nil().to_string(), "thumbprint").is_none());
        assert!(pro_graph_record_id(&id.to_uppercase(), "thumbprint").is_none());
        assert!(pro_graph_record_id(id, "").is_none());
    }
}
