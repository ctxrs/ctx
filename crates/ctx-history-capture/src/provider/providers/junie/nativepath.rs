use std::io::{BufReader, Read, Seek, SeekFrom};

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    provider::normalization::{provider_local_preview, provider_timestamp_millis},
    CaptureError, Result, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    assistant::{
        junie_buffer_result_text, junie_merge_buffered_agent_event, junie_step_output_projection,
        JunieAssistantBuffer, JunieOutputOutcome, JunieStepAgg,
    },
    session_tree::{JunieIndexMeta, JunieSessionPath},
    MAX_JUNIE_TRANSIENT_TURN_BYTES,
};

const MAX_RECORD_SET_ENTRIES: usize = 64;
const RECORD_SET_DIGEST_DOMAIN: &[u8] = b"ctx-junie-jsonl-record-set-v1\0";
const CORE_PAGE_MAX_ROWS: usize = 48;

mod projection;
mod source_backed;

use projection::*;

#[allow(unused_imports)]
pub(crate) use source_backed::{
    JunieLocatorResolverV0, JunieSourceBackedEmissionV0, JunieSourceBackedErrorV0,
    JunieSourceBackedResultV0, JunieSourceBackedScannerV0,
};
