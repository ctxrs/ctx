use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{anyhow, Context, Result};

use ctx_history_core::{utc_now, CaptureProvider};
use ctx_history_store::{
    SourceImportFile, SourceImportInventoryControl as InventoryControl, Store,
};

use crate::commands::import::{provider_path_text, system_time_ms, SourceStats};
use crate::provider_sources::SourceInfo;

mod observation;
mod store_pages;
mod walk;

use store_pages::{InventoryPageStore, SOURCE_IMPORT_STORE_PAGE_SIZE};
use walk::{pace_inventory_page, SourceImportDirectoryWalk};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SourceImportInventory {
    pub(crate) files: usize,
    pub(crate) bytes: u64,
}

#[derive(Debug, Clone, Copy)]
enum InventoryPhase {
    Discovering,
    Reconciling(ReconciliationStage),
    Complete,
}

#[derive(Debug, Clone, Copy)]
enum ReconciliationStage {
    Preference,
    Missing,
}

fn inventory_control_from_source(
    source: &SourceInfo,
    metadata: &fs::Metadata,
    observed_at_ms: i64,
) -> Result<InventoryControl> {
    Ok(InventoryControl::new(
        source.provider,
        source.source_format,
        provider_path_text(&source.path)?,
        metadata.len(),
        system_time_ms(metadata.modified().unwrap_or(UNIX_EPOCH)),
        observed_at_ms,
    ))
}

struct InventoryRun<'a> {
    pages: InventoryPageStore<'a>,
    source: &'a SourceInfo,
    force_reindex: bool,
    observed_at_ms: i64,
}

impl<'a> InventoryRun<'a> {
    fn new(store: &'a Store, source: &'a SourceInfo, force_reindex: bool) -> Result<Self> {
        let source_root = provider_path_text(&source.path)?;
        let observed_at_ms = store.next_source_import_observed_at_ms(
            source.provider,
            source_root,
            utc_now().timestamp_millis(),
        )?;
        Ok(Self {
            pages: InventoryPageStore::new(store),
            source,
            force_reindex,
            observed_at_ms,
        })
    }

    fn run(self, root_metadata: fs::Metadata) -> Result<SourceImportInventory> {
        if root_metadata.file_type().is_file() {
            return self.run_single_file();
        }
        if !root_metadata.file_type().is_dir() {
            return Ok(SourceImportInventory::default());
        }
        self.run_directory(root_metadata)
    }

    fn run_directory(self, root_metadata: fs::Metadata) -> Result<SourceImportInventory> {
        let control =
            inventory_control_from_source(self.source, &root_metadata, self.observed_at_ms)?;
        self.pages.persist_control(
            &control,
            InventoryPhase::Discovering,
            None,
            SourceImportInventory::default(),
        )?;
        let discovered = self.discover(SourceImportDirectoryWalk::new(&self.source.path)?)?;
        self.pages.persist_control(
            &control,
            InventoryPhase::Reconciling(ReconciliationStage::Preference),
            None,
            discovered,
        )?;
        self.reconcile_shadowed(&control, discovered)?;
        self.pages.persist_control(
            &control,
            InventoryPhase::Reconciling(ReconciliationStage::Missing),
            None,
            discovered,
        )?;
        let stale_keyset = self.reconcile_missing(&control, discovered)?;
        self.finish_directory(&control, stale_keyset)
    }

    fn finish_directory(
        &self,
        control: &InventoryControl,
        stale_keyset: Option<String>,
    ) -> Result<SourceImportInventory> {
        let (files, bytes) = self
            .pages
            .source_stats(self.source.provider, control.source_root())?;
        let inventory = SourceImportInventory { files, bytes };
        self.pages.persist_control(
            control,
            InventoryPhase::Complete,
            stale_keyset.as_deref(),
            inventory,
        )?;
        Ok(inventory)
    }

    fn run_single_file(&self) -> Result<SourceImportInventory> {
        let source_root = provider_path_text(&self.source.path)?.to_owned();
        let mut inventory = SourceImportInventory::default();
        if source_import_file_matches(self.source, &self.source.path) {
            let metadata = fs::metadata(&self.source.path).with_context(|| {
                format!("stat import source file {}", self.source.path.display())
            })?;
            let file = observation::source_import_file(
                self.source,
                &self.source.path,
                &metadata,
                self.observed_at_ms,
            )?;
            self.pages
                .persist_data_page(std::slice::from_ref(&file), self.force_reindex)?;
            inventory.files = 1;
            inventory.bytes = metadata.len();
        }
        let mut after_source_path = None;
        while let Some(next) = self.pages.reconcile_single_file_missing_page(
            self.source.provider,
            &source_root,
            self.observed_at_ms,
            after_source_path.as_deref(),
        )? {
            after_source_path = Some(next);
        }
        Ok(inventory)
    }

    fn discover<I>(&self, paths: I) -> Result<SourceImportInventory>
    where
        I: IntoIterator<Item = Result<PathBuf>>,
    {
        let mut inventory = SourceImportInventory::default();
        let mut page = Vec::with_capacity(SOURCE_IMPORT_STORE_PAGE_SIZE);
        for path in paths {
            let path = path?;
            if !source_import_file_matches(self.source, &path) {
                continue;
            }
            let metadata = fs::metadata(&path)
                .with_context(|| format!("stat import source file {}", path.display()))?;
            inventory.files += 1;
            inventory.bytes = inventory.bytes.saturating_add(metadata.len());
            page.push(observation::source_import_file(
                self.source,
                &path,
                &metadata,
                self.observed_at_ms,
            )?);
            if page.len() == SOURCE_IMPORT_STORE_PAGE_SIZE {
                self.pages.persist_data_page(&page, self.force_reindex)?;
                page.clear();
            }
        }
        self.pages.persist_data_page(&page, self.force_reindex)?;
        Ok(inventory)
    }

    fn reconcile_shadowed(
        &self,
        control: &InventoryControl,
        inventory: SourceImportInventory,
    ) -> Result<()> {
        let mut after_source_path = None;
        loop {
            let next = self.pages.reconcile_shadowed_page(
                control,
                inventory,
                after_source_path.as_deref(),
            )?;
            let Some(next) = next else {
                return Ok(());
            };
            after_source_path = Some(next);
            pace_inventory_page();
        }
    }

    fn reconcile_missing(
        &self,
        control: &InventoryControl,
        inventory: SourceImportInventory,
    ) -> Result<Option<String>> {
        let mut after_source_path = None;
        loop {
            let next = self.pages.reconcile_missing_page(
                control,
                inventory,
                after_source_path.as_deref(),
            )?;
            let Some(next) = next else {
                return Ok(after_source_path);
            };
            after_source_path = Some(next);
            pace_inventory_page();
        }
    }
}

pub(crate) fn inventory_source_import_files(
    store: &Store,
    source: &SourceInfo,
    force_reindex: bool,
) -> Result<SourceImportInventory> {
    let root_metadata = fs::symlink_metadata(&source.path)
        .with_context(|| format!("stat import source {}", source.path.display()))?;
    if root_metadata.file_type().is_symlink() {
        return Err(anyhow!(
            "symlinked provider transcript roots are rejected: {}",
            source.path.display()
        ));
    }
    InventoryRun::new(store, source, force_reindex)?.run(root_metadata)
}

pub(crate) fn bounded_source_root_stats(path: &Path) -> Result<SourceStats> {
    walk::bounded_source_root_stats(path)
}

pub(crate) fn persist_source_import_page(store: &Store, files: &[SourceImportFile]) -> Result<()> {
    InventoryPageStore::new(store).persist_data_page(files, false)
}

pub(crate) fn persist_source_import_files(
    store: &Store,
    source: &SourceInfo,
    files: &[SourceImportFile],
) -> Result<()> {
    InventoryPageStore::new(store).persist_files_and_mark_missing(source, files)
}

pub(crate) fn source_uses_import_file_manifest(source: &SourceInfo) -> bool {
    !matches!(
        source.source_format,
        "codex_session_jsonl_tree"
            | "openclaw_session_jsonl_tree"
            | "hermes_state_sqlite"
            | "nanoclaw_project"
            | "astrbot_data_v4_sqlite"
            | "shelley_sqlite"
            | "cline_task_directory_json"
            | "roo_task_directory_json"
            | "firebender_chat_history_sqlite"
            | "codebuddy_history_json"
    )
}

pub(crate) fn source_import_file_matches(source: &SourceInfo, path: &Path) -> bool {
    match source.provider {
        CaptureProvider::Codex | CaptureProvider::Pi | CaptureProvider::FactoryAiDroid => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        }
        CaptureProvider::Claude => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path.starts_with(&source.path)
        }
        CaptureProvider::OpenCode
        | CaptureProvider::Kilo
        | CaptureProvider::MiMoCode
        | CaptureProvider::KiroCli
        | CaptureProvider::ForgeCode
        | CaptureProvider::DeepAgents
        | CaptureProvider::Crush
        | CaptureProvider::Goose
        | CaptureProvider::Lingma
        | CaptureProvider::Warp
        | CaptureProvider::Zed => path == source.path,
        CaptureProvider::MistralVibe => {
            path == source.path
                || (path.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl")
                    && path.starts_with(&source.path))
        }
        CaptureProvider::Mux => {
            path == source.path
                || (matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some("chat.jsonl" | "partial.json")
                ) && path.starts_with(&source.path))
        }
        CaptureProvider::RovoDev => {
            path.file_name().and_then(|name| name.to_str()) == Some("session_context.json")
        }
        CaptureProvider::CopilotCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("events.jsonl")
        }
        CaptureProvider::Antigravity => matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("transcript_full.jsonl" | "transcript.jsonl")
        ),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "chats")
        }
        CaptureProvider::Cursor => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "agent-transcripts")
        }
        CaptureProvider::Windsurf => path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"),
        CaptureProvider::Qoder => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "transcript")
        }
        CaptureProvider::Continue => {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path.file_name().and_then(|name| name.to_str()) != Some("sessions.json")
        }
        CaptureProvider::QwenCode => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "chats")
        }
        CaptureProvider::CodeBuddy => {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "history")
        }
        CaptureProvider::Trae => {
            path.file_name().and_then(|name| name.to_str()) == Some("state.vscdb")
                && (path == source.path || path.starts_with(&source.path))
        }
        CaptureProvider::KimiCodeCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("wire.jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "agents")
        }
        CaptureProvider::Auggie => {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path.starts_with(&source.path)
        }
        CaptureProvider::Junie => {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "events.jsonl")
                && path.starts_with(&source.path)
        }
        CaptureProvider::Firebender => {
            path.file_name().and_then(|name| name.to_str()) == Some("chat_history.db")
                && (path == source.path || path.starts_with(&source.path))
        }
        CaptureProvider::OpenClaw => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path.starts_with(&source.path)
        }
        CaptureProvider::OpenHands => {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path.starts_with(&source.path)
                && path
                    .components()
                    .any(|component| component.as_os_str() == "v1_conversations")
        }
        CaptureProvider::Hermes
        | CaptureProvider::NanoClaw
        | CaptureProvider::AstrBot
        | CaptureProvider::Shelley
        | CaptureProvider::Cline
        | CaptureProvider::RooCode
        | CaptureProvider::Shell
        | CaptureProvider::Git
        | CaptureProvider::Jj
        | CaptureProvider::Gh
        | CaptureProvider::Custom
        | CaptureProvider::Unknown => false,
    }
}

#[cfg(test)]
#[path = "manifest/lifecycle_tests.rs"]
mod lifecycle_tests;
