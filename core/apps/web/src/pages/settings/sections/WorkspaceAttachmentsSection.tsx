import { Ellipsis, Plus, RefreshCw, X } from "lucide-react";
import { useMemo, useState } from "react";
import { guessAttachmentName } from "../SettingsPage.utils";
import { formatAttachmentStatus } from "../SettingsPage.helpers";
import { idToString, type WorkspaceAttachment } from "../../../api/client";
import { ExternalLink } from "../../../components/ExternalLink";
import { TextInput } from "../../../components/ui/text-input";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../../components/ui/dropdown-menu";
import { useWorkspaceAttachmentsController } from "../hooks/useWorkspaceAttachmentsController";
import { GeneralSection } from "./GeneralSection";

type WorkspaceAttachmentsSectionProps = {
  workspaceId: string | null;
  active: boolean;
};

type AttachmentModalKind = "reference_repo" | "doc_mirror" | null;

const sourceToOpenHref = (source: string): string | null => {
  const trimmed = source.trim();
  if (!trimmed) return null;

  try {
    const parsed = new URL(trimmed);
    if (parsed.protocol === "http:" || parsed.protocol === "https:") {
      return parsed.toString();
    }
  } catch {
    // ignore non-URL inputs
  }

  const githubSsh = /^git@github\.com:(.+?)(?:\.git)?$/i.exec(trimmed);
  if (githubSsh?.[1]) {
    return `https://github.com/${githubSsh[1]}`;
  }

  return null;
};

const attachmentStatusDotClass = (status: WorkspaceAttachment["status"]): string => {
  if (status === "ready") return "settings-attachments-status-dot-ready";
  if (status === "error") return "settings-attachments-status-dot-error";
  if (status === "syncing") return "settings-attachments-status-dot-syncing";
  return "settings-attachments-status-dot-pending";
};

const formatIndexedAgeLabel = (ms: number): string => {
  if (!Number.isFinite(ms)) return "just now";
  const safeMs = Math.max(0, ms);
  const minuteMs = 60_000;
  const hourMs = 60 * minuteMs;
  const dayMs = 24 * hourMs;
  const monthMs = 30 * dayMs;
  const yearMs = 365 * dayMs;

  if (safeMs < minuteMs) return "just now";
  if (safeMs < hourMs) return `${Math.floor(safeMs / minuteMs)}m ago`;
  if (safeMs < dayMs) return `${Math.floor(safeMs / hourMs)}h ago`;
  if (safeMs < monthMs) return `${Math.floor(safeMs / dayMs)}d ago`;
  if (safeMs < yearMs) {
    const months = Math.floor(safeMs / monthMs);
    return `${months} month${months === 1 ? "" : "s"} ago`;
  }
  const years = Math.floor(safeMs / yearMs);
  return `${years} year${years === 1 ? "" : "s"} ago`;
};

const attachmentIndexedLabel = (attachment: WorkspaceAttachment): string => {
  const now = Date.now();
  const updatedMs = Date.parse(attachment.updated_at);
  const lastSyncMs = attachment.last_sync_at ? Date.parse(attachment.last_sync_at) : Number.NaN;

  if (attachment.status === "ready") {
    const age = formatIndexedAgeLabel(now - (Number.isFinite(lastSyncMs) ? lastSyncMs : updatedMs));
    return `Indexed ${age}`;
  }
  if (attachment.status === "error") {
    const age = formatIndexedAgeLabel(now - updatedMs);
    return `Failed ${age}`;
  }
  if (attachment.status === "syncing") {
    const age = formatIndexedAgeLabel(now - updatedMs);
    return `Indexing ${age}`;
  }
  const age = formatIndexedAgeLabel(now - updatedMs);
  return `Queued ${age}`;
};

export function WorkspaceAttachmentsSection({ workspaceId, active }: WorkspaceAttachmentsSectionProps) {
  const {
    attachmentSource,
    setAttachmentSource,
    attachmentName,
    setAttachmentName,
    attachmentRevision,
    setAttachmentRevision,
    handleAddAttachment,
    attachmentBusy,
    syncWorkspaceAttachmentsNow,
    attachmentSyncBusy,
    attachmentsLoading,
    attachments,
    attachmentDeleteBusy,
    handleRemoveAttachment,
    docsAttachmentSource,
    setDocsAttachmentSource,
    docsAttachmentName,
    setDocsAttachmentName,
    handleAddDocsAttachment,
    docsAttachmentBusy,
    attachmentsError,
  } = useWorkspaceAttachmentsController({
    workspaceId,
    enabled: active,
  });

  const [modalKind, setModalKind] = useState<AttachmentModalKind>(null);
  const [editingAttachmentId, setEditingAttachmentId] = useState<string | null>(null);

  const canAdd = Boolean(workspaceId && attachmentSource.trim());
  const canAddDocs = Boolean(workspaceId && docsAttachmentSource.trim());
  const referenceAttachments = useMemo(
    () => attachments.filter((attachment) => attachment.kind === "reference_repo"),
    [attachments],
  );
  const docsAttachments = useMemo(() => attachments.filter((attachment) => attachment.kind === "doc_mirror"), [attachments]);
  const editingAttachment = useMemo(
    () => attachments.find((attachment) => idToString(attachment.id) === editingAttachmentId) ?? null,
    [attachments, editingAttachmentId],
  );
  const isRepoEdit = modalKind === "reference_repo" && editingAttachment?.kind === "reference_repo";
  const isDocsEdit = modalKind === "doc_mirror" && editingAttachment?.kind === "doc_mirror";

  const closeModal = () => {
    if (attachmentBusy || docsAttachmentBusy) return;
    setModalKind(null);
    setEditingAttachmentId(null);
  };

  const openRepoModal = () => {
    setEditingAttachmentId(null);
    setAttachmentSource("");
    setAttachmentName("");
    setAttachmentRevision("");
    setModalKind("reference_repo");
  };

  const openDocsModal = () => {
    setEditingAttachmentId(null);
    setDocsAttachmentSource("");
    setDocsAttachmentName("");
    setModalKind("doc_mirror");
  };

  const openEditModal = (attachment: WorkspaceAttachment) => {
    const id = idToString(attachment.id);
    setEditingAttachmentId(id);
    if (attachment.kind === "reference_repo") {
      setAttachmentSource(attachment.source);
      setAttachmentName(attachment.name);
      setAttachmentRevision(attachment.revision ?? "");
      setModalKind("reference_repo");
      return;
    }
    setDocsAttachmentSource(attachment.source);
    setDocsAttachmentName(attachment.name);
    setModalKind("doc_mirror");
  };

  const renderAttachmentRow = (attachment: WorkspaceAttachment) => {
    const updatedLabel = attachmentIndexedLabel(attachment);
    const statusLabel = formatAttachmentStatus(attachment.status);
    const statusTitle = attachment.error_message ? `${statusLabel}: ${attachment.error_message}` : statusLabel;
    const deleteBusy = attachmentDeleteBusy[idToString(attachment.id)] ?? false;
    const openHref = sourceToOpenHref(attachment.source);

    return (
      <div key={idToString(attachment.id)} className="settings-attachments-list-row">
        <div className="settings-attachments-list-main">
          <div className="settings-attachments-list-title-row">
            <span
              className={`settings-attachments-status-dot ${attachmentStatusDotClass(attachment.status)}`}
              title={statusTitle}
              aria-hidden="true"
            />
            <div className="settings-table-title settings-attachments-title-truncate">{attachment.name}</div>
          </div>
          {openHref ? (
            <ExternalLink
              className="settings-table-mono settings-attachments-source settings-attachments-source-link"
              href={openHref}
              title={attachment.source}
            >
              {attachment.source}
            </ExternalLink>
          ) : (
            <div className="settings-table-mono settings-attachments-source" title={attachment.source}>
              {attachment.source}
            </div>
          )}
        </div>
        <div className="settings-attachments-list-actions">
          <span className="settings-table-sub settings-attachments-indexed-label">{updatedLabel}</span>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button
                type="button"
                className="settings-attachments-row-menu-trigger"
                aria-label={`More actions for ${attachment.name}`}
                title="More actions"
              >
                <Ellipsis size={14} aria-hidden="true" />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem
                onSelect={() => {
                  openEditModal(attachment);
                }}
              >
                Edit
              </DropdownMenuItem>
              <DropdownMenuItem
                disabled={!workspaceId || attachmentSyncBusy}
                onSelect={() => {
                  void syncWorkspaceAttachmentsNow();
                }}
              >
                {attachmentSyncBusy ? "Refreshing..." : "Refresh"}
              </DropdownMenuItem>
              <DropdownMenuItem
                className="tw-text-[var(--error-contrast)] focus:tw-bg-[var(--error-soft)]"
                disabled={deleteBusy}
                onSelect={() => {
                  void handleRemoveAttachment(attachment);
                }}
              >
                {deleteBusy ? "Removing..." : "Delete"}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>
    );
  };

  return (
    <GeneralSection>
      <div className="settings-preferences-flat">
        <div className="settings-preferences-group">
          <div className="settings-row settings-row-stack">
            <div className="settings-row-inline-head">
              <div className="settings-row-left">
                <div className="settings-row-title">Reference repos</div>
                <div className="settings-row-desc settings-row-desc-balance">
                  Add read-only repositories as workspace attachments to give agents extra context. Use this for
                  related internal repos, upstream libraries, or prior-art codebases.
                </div>
              </div>
              <div className="settings-attachments-title-actions">
                <button
                  type="button"
                  className="settings-attachments-icon-btn"
                  onClick={() => {
                    void syncWorkspaceAttachmentsNow();
                  }}
                  disabled={!workspaceId || attachmentSyncBusy}
                  aria-label="Refresh attachments"
                  title="Refresh attachments"
                >
                  <RefreshCw size={15} className={attachmentSyncBusy ? "settings-autosave-spin" : ""} aria-hidden="true" />
                </button>
                <button
                  type="button"
                  className="settings-attachments-icon-btn"
                  onClick={openRepoModal}
                  disabled={!workspaceId}
                  aria-label="Add reference repo"
                  title="Add reference repo"
                >
                  <Plus size={16} aria-hidden="true" />
                </button>
              </div>
            </div>
            <div className="settings-row-field settings-attachments-panel">
              {attachmentsLoading ? <div className="settings-empty-compact">Loading attachments…</div> : null}
              {!attachmentsLoading && referenceAttachments.length === 0 ? (
                <div className="settings-empty-compact">No reference repos yet.</div>
              ) : null}
              {!attachmentsLoading && referenceAttachments.length > 0 ? (
                <div className="settings-attachments-list">{referenceAttachments.map(renderAttachmentRow)}</div>
              ) : null}
            </div>
          </div>
        </div>

        <div className="settings-preferences-group">
          <div className="settings-row settings-row-stack">
            <div className="settings-row-inline-head">
              <div className="settings-row-left">
                <div className="settings-row-title">Indexed Docs</div>
                <div className="settings-row-desc settings-row-desc-balance">
                  Mirror docs as markdown and make them available in every worktree. Agents can reference docs directly
                  from the shell without parsing HTML.
                </div>
              </div>
              <div className="settings-attachments-title-actions">
                <button
                  type="button"
                  className="settings-attachments-icon-btn"
                  onClick={() => {
                    void syncWorkspaceAttachmentsNow();
                  }}
                  disabled={!workspaceId || attachmentSyncBusy}
                  aria-label="Refresh docs attachments"
                  title="Refresh docs attachments"
                >
                  <RefreshCw size={15} className={attachmentSyncBusy ? "settings-autosave-spin" : ""} aria-hidden="true" />
                </button>
                <button
                  type="button"
                  className="settings-attachments-icon-btn"
                  onClick={openDocsModal}
                  disabled={!workspaceId}
                  aria-label="Add docs mirror"
                  title="Add docs mirror"
                >
                  <Plus size={16} aria-hidden="true" />
                </button>
              </div>
            </div>
            <div className="settings-row-field settings-attachments-panel">
              {attachmentsLoading ? <div className="settings-empty-compact">Loading attachments…</div> : null}
              {!attachmentsLoading && docsAttachments.length === 0 ? (
                <div className="settings-empty-compact">No docs mirrors yet.</div>
              ) : null}
              {!attachmentsLoading && docsAttachments.length > 0 ? (
                <div className="settings-attachments-list">{docsAttachments.map(renderAttachmentRow)}</div>
              ) : null}
            </div>
          </div>
        </div>
      </div>
      {modalKind ? (
        <div className="modal-overlay" role="dialog" aria-modal="true" onClick={closeModal}>
          <div className="modal settings-harness-modal settings-attachments-modal" onClick={(e) => e.stopPropagation()}>
            <div className="settings-harness-modal-header">
              <div className="settings-main-title settings-info-modal-title">
                {modalKind === "reference_repo"
                  ? isRepoEdit
                    ? "Edit reference repo"
                    : "Add reference repo"
                  : isDocsEdit
                    ? "Edit docs mirror"
                    : "Add docs mirror"}
              </div>
              <button
                type="button"
                className="settings-harness-modal-close"
                onClick={closeModal}
                aria-label="Close"
                disabled={attachmentBusy || docsAttachmentBusy}
              >
                <X size={16} aria-hidden="true" />
              </button>
            </div>

            {modalKind === "reference_repo" ? (
              <form
                className="settings-harness-modal-fields"
                onSubmit={(event) => {
                  event.preventDefault();
                  void (async () => {
                    const added = await handleAddAttachment();
                    if (added) {
                      setModalKind(null);
                      setEditingAttachmentId(null);
                    }
                  })();
                }}
              >
                <label className="settings-harness-modal-label" htmlFor="attachments-source">
                  Repository URL
                  <TextInput
                    id="attachments-source"
                    className="settings-control settings-control-wide"
                    value={attachmentSource}
                    onChange={(e) => setAttachmentSource(e.target.value)}
                    placeholder="git@github.com:org/repo.git"
                  />
                </label>
                <label className="settings-harness-modal-label" htmlFor="attachments-name">
                  Display name
                  <TextInput
                    id="attachments-name"
                    className="settings-control settings-control-wide"
                    value={attachmentName}
                    onChange={(e) => setAttachmentName(e.target.value)}
                    placeholder={guessAttachmentName(attachmentSource) || "reference"}
                    readOnly={isRepoEdit}
                  />
                </label>
                <label className="settings-harness-modal-label" htmlFor="attachments-revision">
                  Revision (optional)
                  <TextInput
                    id="attachments-revision"
                    className="settings-control settings-control-wide"
                    value={attachmentRevision}
                    onChange={(e) => setAttachmentRevision(e.target.value)}
                    placeholder="main or tag"
                  />
                </label>
                <div className="modal-actions settings-harness-modal-actions">
                  <button
                    type="button"
                    className="settings-btn settings-btn-secondary"
                    onClick={closeModal}
                    disabled={attachmentBusy}
                  >
                    Cancel
                  </button>
                  <button type="submit" className="settings-btn" disabled={!canAdd || attachmentBusy || !workspaceId}>
                    {attachmentBusy ? (isRepoEdit ? "Saving…" : "Adding…") : isRepoEdit ? "Save" : "Add repo"}
                  </button>
                </div>
              </form>
            ) : (
              <form
                className="settings-harness-modal-fields"
                onSubmit={(event) => {
                  event.preventDefault();
                  void (async () => {
                    const added = await handleAddDocsAttachment();
                    if (added) {
                      setModalKind(null);
                      setEditingAttachmentId(null);
                    }
                  })();
                }}
              >
                <label className="settings-harness-modal-label" htmlFor="attachments-docs-source">
                  Docs URL
                  <TextInput
                    id="attachments-docs-source"
                    className="settings-control settings-control-wide"
                    value={docsAttachmentSource}
                    onChange={(e) => setDocsAttachmentSource(e.target.value)}
                    placeholder="https://docs.example.com/"
                  />
                </label>
                <label className="settings-harness-modal-label" htmlFor="attachments-docs-name">
                  Display name
                  <TextInput
                    id="attachments-docs-name"
                    className="settings-control settings-control-wide"
                    value={docsAttachmentName}
                    onChange={(e) => setDocsAttachmentName(e.target.value)}
                    placeholder={guessAttachmentName(docsAttachmentSource) || "docs"}
                    readOnly={isDocsEdit}
                  />
                </label>
                <div className="modal-actions settings-harness-modal-actions">
                  <button
                    type="button"
                    className="settings-btn settings-btn-secondary"
                    onClick={closeModal}
                    disabled={docsAttachmentBusy}
                  >
                    Cancel
                  </button>
                  <button type="submit" className="settings-btn" disabled={!canAddDocs || docsAttachmentBusy || !workspaceId}>
                    {docsAttachmentBusy ? (isDocsEdit ? "Saving…" : "Adding…") : isDocsEdit ? "Save" : "Add docs"}
                  </button>
                </div>
              </form>
            )}
          </div>
        </div>
      ) : null}
      {attachmentsError ? <div className="settings-banner settings-banner-error">{attachmentsError}</div> : null}
    </GeneralSection>
  );
}
