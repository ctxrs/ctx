import type React from "react";

import { DictationOnboardingModal } from "../../components/dictation/DictationOnboardingModal";
import {
  WorkbenchComposer,
  type DraftHarness,
  type WorkbenchComposerProps,
  type WorkbenchModeId,
} from "../../components/WorkbenchComposer";
import type { MessageAttachment } from "../../api/client";
import type { SlashCommandDescriptor } from "../../state/useComposerAutocomplete";
import { HARNESS_CATALOG } from "../../utils/harnessCatalog";
type WorkbenchNewSessionProps = Extract<WorkbenchComposerProps, { variant: "newSession" }>;

type WorkbenchEmptyStateProps = {
  newComposerRef: React.Ref<HTMLDivElement>;
  dropActive: boolean;
  draftPrompt: string;
  setDraftPrompt: (value: string) => void;
  dictationRecording: boolean;
  onToggleRecording: () => void;
  workspaceId: string;
  slashCommands: SlashCommandDescriptor[];
  draftAttachments: MessageAttachment[];
  setDraftAttachments: React.Dispatch<React.SetStateAction<MessageAttachment[]>>;
  onAttachmentError: (message: string | null) => void;
  onSend: () => Promise<void>;
  sendDisabled: boolean;
  sendDisabledReason: string | null;
  draftMode: WorkbenchModeId;
  setDraftMode: (modeId: WorkbenchModeId) => void;
  providersById: WorkbenchNewSessionProps["providersById"];
  providerInstallsById: WorkbenchNewSessionProps["providerInstallsById"];
  onInstallProvider: WorkbenchNewSessionProps["onInstallProvider"];
  onCancelInstallProvider: WorkbenchNewSessionProps["onCancelInstallProvider"];
  onInstallAllProviders: WorkbenchNewSessionProps["onInstallAllProviders"];
  installAllBusy: boolean;
  providerOptions: WorkbenchNewSessionProps["providerOptions"];
  ensureProviderAuthSummary: WorkbenchNewSessionProps["ensureProviderAuthSummary"];
  onRequestHarnessAuth: WorkbenchNewSessionProps["onRequestHarnessAuth"];
  draftHarness: DraftHarness | null;
  setDraftHarness: React.Dispatch<React.SetStateAction<DraftHarness | null>>;
  defaultProviderId: WorkbenchNewSessionProps["defaultProviderId"];
  dictationDebugText: string | null;
  dictationError: string | null;
  startError: string | null;
  dictationOnboarding: React.ComponentProps<typeof DictationOnboardingModal>["state"];
  onCloseDictationOnboarding: () => void;
  onBackDictationOnboarding: () => void;
  onChooseDictationOnboardingLocal: () => void;
  onChooseDictationOnboardingCloud: () => void;
  onCloudChangeDictationOnboarding: React.ComponentProps<typeof DictationOnboardingModal>["onCloudChange"];
  onSubmitCloudDictationOnboarding: () => void;
  onSubmitLocalDictationOnboarding: () => void;
};

export function WorkbenchEmptyState({
  newComposerRef,
  dropActive,
  draftPrompt,
  setDraftPrompt,
  dictationRecording,
  onToggleRecording,
  workspaceId,
  slashCommands,
  draftAttachments,
  setDraftAttachments,
  onAttachmentError,
  onSend,
  sendDisabled,
  sendDisabledReason,
  draftMode,
  setDraftMode,
  providersById,
  providerInstallsById,
  onInstallProvider,
  onCancelInstallProvider,
  onInstallAllProviders,
  installAllBusy,
  providerOptions,
  ensureProviderAuthSummary,
  onRequestHarnessAuth,
  draftHarness,
  setDraftHarness,
  defaultProviderId,
  dictationDebugText,
  dictationError,
  startError,
  dictationOnboarding,
  onCloseDictationOnboarding,
  onBackDictationOnboarding,
  onChooseDictationOnboardingLocal,
  onChooseDictationOnboardingCloud,
  onCloudChangeDictationOnboarding,
  onSubmitCloudDictationOnboarding,
  onSubmitLocalDictationOnboarding,
}: WorkbenchEmptyStateProps) {
  return (
    <div className="wb-center">
      <div className="wb-new-composer-stack ctx-drop-scope" ref={newComposerRef}>
        {dropActive ? (
          <div className="ctx-drop-overlay" aria-hidden="true">
            <div className="ctx-drop-overlay-text">Drop image to attach</div>
          </div>
        ) : null}
        <WorkbenchComposer
          variant="newSession"
          value={draftPrompt}
          setValue={setDraftPrompt}
          placeholder="@ for context, / for commands"
          inputDisabled={dictationRecording}
          recording={dictationRecording}
          onToggleRecording={onToggleRecording}
          sessionIdForAutocomplete={null}
          workspaceIdForAutocomplete={workspaceId}
          slashCommands={slashCommands}
          attachments={draftAttachments}
          setAttachments={setDraftAttachments}
          onAttachmentError={onAttachmentError}
          onSend={onSend}
          sendDisabled={sendDisabled}
          sendDisabledReason={sendDisabledReason}
          onInterrupt={null}
          modeId={draftMode}
          setModeId={setDraftMode}
          harnessCatalog={HARNESS_CATALOG}
          providersById={providersById}
          providerInstallsById={providerInstallsById}
          onInstallProvider={onInstallProvider}
          onCancelInstallProvider={onCancelInstallProvider}
          onInstallAllProviders={onInstallAllProviders}
          installAllBusy={installAllBusy}
          providerOptions={providerOptions}
          ensureProviderAuthSummary={ensureProviderAuthSummary}
          onRequestHarnessAuth={onRequestHarnessAuth}
          draftHarness={draftHarness}
          setDraftHarness={setDraftHarness}
          defaultProviderId={defaultProviderId}
        />
        {dictationDebugText ? <div className="wb-banner">{dictationDebugText}</div> : null}
        {dictationError ? <div className="wb-banner">{dictationError}</div> : null}
        {startError ? <div className="wb-banner">{startError}</div> : null}
        <DictationOnboardingModal
          state={dictationOnboarding}
          onClose={onCloseDictationOnboarding}
          onBack={onBackDictationOnboarding}
          onChooseLocal={onChooseDictationOnboardingLocal}
          onChooseCloud={onChooseDictationOnboardingCloud}
          onCloudChange={onCloudChangeDictationOnboarding}
          onSubmitCloud={onSubmitCloudDictationOnboarding}
          onSubmitLocal={onSubmitLocalDictationOnboarding}
        />
      </div>
    </div>
  );
}
