import type { UpdateCheck } from "../../api/client";

export type UpdateNoticeBannerProps = {
  allTasksIdle?: boolean;
};

export type UpdateNoticeBannerModel = {
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
