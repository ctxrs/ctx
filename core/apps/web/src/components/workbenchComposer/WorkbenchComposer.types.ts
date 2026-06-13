import type React from "react";
import type {
  InstallErrorCode,
  InstallTarget,
  MessageAttachment,
  ProviderOptions,
  ProviderStatus,
} from "../../api/client";
import type { SlashCommandDescriptor } from "../../state/useComposerAutocomplete";
import type { SessionViewVerbosity } from "../../state/uiStateStore";
import type { HarnessCatalogEntry } from "../../utils/harnessCatalog";

export type WorkbenchModeId = "default" | "research" | "plan" | "review";
export type ProviderAuthSummaryTrigger = "passive" | "explicit";
export type ContextWindowInfo = {
  windowTokens?: number;
  usedTokens?: number;
  remainingTokens?: number;
  remainingFraction?: number;
};

export type DraftHarness = {
  providerId: string;
  modelId: string;
  preferenceExplicit?: boolean;
};

type SharedProps = {
  variant: "newSession" | "activeSession";

  value: string;
  setValue: (next: string) => void;
  placeholder: string;
  inputDisabled?: boolean;

  sessionIdForAutocomplete: string | null;
  workspaceIdForAutocomplete?: string | null;
  slashCommands: SlashCommandDescriptor[];

  attachments: MessageAttachment[];
  setAttachments: React.Dispatch<React.SetStateAction<MessageAttachment[]>>;
  onAttachmentError?: (message: string | null) => void;

  onSend: () => void;
  sendDisabledReason?: string | null;
  sendDisabled?: boolean;

  onInterrupt?: (() => void) | null;
  isWorking?: boolean;
  interruptPending?: boolean;
  verbosity?: SessionViewVerbosity;
  onSetVerbosity?: (next: SessionViewVerbosity) => void;

  modeId: WorkbenchModeId;
  setModeId: (next: WorkbenchModeId) => void;

  recording?: boolean;
  recordDisabledReason?: string | null;
  onToggleRecording?: (() => void) | null;
};

export type NewSessionProps = SharedProps & {
  variant: "newSession";
  harnessCatalog: HarnessCatalogEntry[];
  providersById: Record<string, ProviderStatus>;
  providerInstallsById: Record<
    string,
    | {
        installId: string;
        state: "running" | "succeeded" | "failed" | "cancelled";
        pct: number | null;
        target?: InstallTarget;
        errorCode?: InstallErrorCode;
        error?: string;
      }
    | undefined
  >;
  onInstallProvider: (providerId: string) => void;
  onCancelInstallProvider?: (providerId: string) => void;
  onInstallAllProviders: () => void;
  installAllBusy?: boolean;
  providerOptions: Record<string, ProviderOptions | undefined>;
  ensureProviderAuthSummary: (
    providerId: string,
    opts?: { force?: boolean; trigger?: ProviderAuthSummaryTrigger },
  ) => Promise<ProviderOptions | undefined>;
  onRequestHarnessAuth?: (providerId: string) => void;

  draftHarness: DraftHarness | null;
  setDraftHarness: React.Dispatch<React.SetStateAction<DraftHarness | null>>;
  defaultProviderId: string;
};

export type ActiveSessionProps = SharedProps & {
  variant: "activeSession";
  providerId?: string;
  harnessLabel: string;
  harnessLogoSrc?: string;
  harnessLogoInvert?: boolean;
  harnessLogoInvertInLight?: boolean;

  availableModels: Array<{ id: string; name?: string }>;
  currentModelId: string;
  currentModelDisplayLabel?: string;
  onSetModelId: (next: string) => void;

  contextWindow?: ContextWindowInfo | null;
};

export type WorkbenchComposerProps = NewSessionProps | ActiveSessionProps;
