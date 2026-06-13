import { Info, X } from "lucide-react";
import type { UpdateCheck } from "../../api/client";
import { ExternalLink } from "../ExternalLink";

type UpdateNoticeBannerViewProps = {
  applyingUpdate: boolean;
  canDismissBanner: boolean;
  effectiveError: string | null;
  forcedUpdate: boolean;
  latest: string;
  latestKnownVersion: string;
  minimumSupportedVersion: string;
  releaseNotesUrl: string;
  restartRequired: boolean;
  shouldRenderBanner: boolean;
  showInfoModal: boolean;
  showUpdateActions: boolean;
  snackbarTitle: string;
  updateActionDisabled: boolean;
  updateActionLabel: string;
  updateError: string | null;
  updateInfo: UpdateCheck | null;
  updateStatus: string | null;
  dismissForLater: () => void;
  onForcedUpdateNow: () => void;
  onRestartNow: () => void;
  onUpdateNow: () => void;
  openInfoModal: () => void;
  closeInfoModal: () => void;
  requestUpdateOnNextIdle: () => void;
};

export function UpdateNoticeBannerView({
  applyingUpdate,
  canDismissBanner,
  effectiveError,
  forcedUpdate,
  latest,
  latestKnownVersion,
  minimumSupportedVersion,
  releaseNotesUrl,
  restartRequired,
  shouldRenderBanner,
  showInfoModal,
  showUpdateActions,
  snackbarTitle,
  updateActionDisabled,
  updateActionLabel,
  updateError,
  updateInfo,
  updateStatus,
  dismissForLater,
  onForcedUpdateNow,
  onRestartNow,
  onUpdateNow,
  openInfoModal,
  closeInfoModal,
  requestUpdateOnNextIdle,
}: UpdateNoticeBannerViewProps) {
  if (!forcedUpdate && !shouldRenderBanner && !showInfoModal) return null;

  return (
    <>
      {forcedUpdate ? (
        <div
          className="daemon-overlay wb-update-required-overlay"
          role="dialog"
          aria-modal="true"
          aria-label="Update required"
        >
          <div className="daemon-overlay-card wb-update-required-card">
            <div className="daemon-overlay-eyebrow">Update required</div>
            <h2>Update required to continue</h2>
            <p className="daemon-overlay-body">
              This version is no longer supported. Install the latest update now to continue using
              ctx.
            </p>
            <div className="daemon-overlay-target">
              Installed:{" "}
              <span className="daemon-overlay-mono">
                {updateInfo?.current_version ?? "unknown"}
              </span>
            </div>
            <div className="daemon-overlay-target">
              Minimum supported:{" "}
              <span className="daemon-overlay-mono">
                {minimumSupportedVersion || "unknown"}
              </span>
            </div>
            {latestKnownVersion ? (
              <div className="daemon-overlay-target">
                Latest: <span className="daemon-overlay-mono">{latestKnownVersion}</span>
              </div>
            ) : null}
            {updateError ? <div className="daemon-overlay-error">{updateError}</div> : null}
            <div className="daemon-overlay-actions">
              <button
                type="button"
                className="daemon-overlay-button"
                disabled={updateActionDisabled}
                onClick={restartRequired ? onRestartNow : onForcedUpdateNow}
              >
                {updateActionLabel}
              </button>
            </div>
          </div>
        </div>
      ) : shouldRenderBanner ? (
        <div
          className="wb-snackbar wb-update-snackbar"
          role="status"
          aria-live="polite"
          data-testid="update-available-snackbar"
        >
          <div className="wb-snackbar-body wb-update-snackbar-body">
            <div className="wb-snackbar-title wb-update-snackbar-title-row">
              <span>{snackbarTitle}</span>
              {showUpdateActions ? (
                <button
                  type="button"
                  className="wb-update-snackbar-info-btn"
                  aria-label="Learn about update timing"
                  title="Learn about update timing"
                  onClick={openInfoModal}
                >
                  <Info size={14} aria-hidden="true" />
                </button>
              ) : null}
            </div>
            {latestKnownVersion ? (
              <div className="wb-snackbar-subtitle">
                <ExternalLink
                  href={releaseNotesUrl}
                  className="wb-update-release-notes-link"
                >
                  View release notes
                </ExternalLink>
              </div>
            ) : null}
            {updateStatus ? <div className="wb-snackbar-subtitle">{updateStatus}</div> : null}
            {effectiveError ? <div className="wb-snackbar-error">{effectiveError}</div> : null}
          </div>
          {showUpdateActions ? (
            <div className="wb-snackbar-actions wb-update-snackbar-actions">
              <button
                type="button"
                className="wb-snackbar-btn"
                disabled={updateActionDisabled}
                onClick={restartRequired ? onRestartNow : onUpdateNow}
              >
                {updateActionLabel}
              </button>
              <button
                type="button"
                className="wb-snackbar-btn wb-snackbar-btn-secondary"
                disabled={applyingUpdate}
                onClick={requestUpdateOnNextIdle}
              >
                Update on Next Idle
              </button>
            </div>
          ) : null}
          {canDismissBanner ? (
            <button
              type="button"
              className="wb-snackbar-close"
              onClick={dismissForLater}
              aria-label="Dismiss update notice"
              disabled={applyingUpdate}
            >
              <X size={14} aria-hidden="true" />
            </button>
          ) : null}
        </div>
      ) : null}
      {showInfoModal ? (
        <div
          className="modal-overlay"
          role="dialog"
          aria-modal="true"
          aria-label="Update timing info"
          onClick={closeInfoModal}
        >
          <div className="modal wb-update-info-modal" onClick={(event) => event.stopPropagation()}>
            <h3>Update timing</h3>
            <p>
              Updating immediately will interrupt any actively running tasks. All your work will be
              saved, but you&apos;ll have to resume any agents directly.
            </p>
            <p>
              If you select to update on next idle, the app will wait for the next time that all
              tasks are idle and then trigger the update.
            </p>
            <div className="modal-actions">
              <button type="button" className="wb-snackbar-btn" onClick={closeInfoModal}>
                Close
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </>
  );
}
