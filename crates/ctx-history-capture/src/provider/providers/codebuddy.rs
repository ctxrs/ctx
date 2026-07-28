mod native_path;
mod normalization;
mod source;

pub(crate) use normalization::{codebuddy_decoded_message, codebuddy_message_text};

const CODEBUDDY_NATIVE_CURSOR_VERSION: u32 = 1;
const CODEBUDDY_CAPTURE_REVISION: u32 = 5;
const CODEBUDDY_POLICY_REVISION: u32 = 6;
const CODEBUDDY_CLI_POLICY_REVISION: u32 = 7;
const CODEBUDDY_MAX_CHECKPOINT_TEXT_BYTES: usize = 8 * 1024;
const CODEBUDDY_MAX_FAILURE_BYTES: usize = 2 * 1024;
const CODEBUDDY_MAX_CHECKPOINT_FAILURES: usize = 64;

pub(crate) use native_path::{
    codebuddy_cli_complete_content_record, codebuddy_cli_complete_content_source_from_admitted,
    hydrate_codebuddy_source_backed_record, import_codebuddy_nativepath,
    scan_codebuddy_source_backed_root, CodeBuddyHydratedSourceRecord, CodeBuddySourceBackedPage,
    CodeBuddySourceBackedRejection, CodeBuddySourceBackedScan,
};
