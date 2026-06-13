import { useCallback, useMemo, useRef } from "react";
import { ArrowUp, ChevronDown, Ellipsis, Image, Square } from "lucide-react";
import { shouldSendOnEnter } from "../../utils/keyboard";
import { formatEffortLabel } from "../../utils/modelEffort";
import { errorMessage } from "../../utils/errorMessage";
import { shouldHydrateProviderModels } from "../../pages/workbenchShell/useWorkbenchProviders";
import { ComposerAutocompleteMenu } from "../ComposerAutocompleteMenu";
import { MessageAttachmentImage } from "../MessageAttachmentImage";
import { useComposerAutocomplete } from "../../state/useComposerAutocomplete";
import { imageFilesToMessageAttachments } from "../../utils/messageAttachments";
import { TextInput, Textarea } from "../ui/text-input";
import type { SessionViewVerbosity } from "../../state/uiStateStore";
import { MenuTitleRow } from "./WorkbenchComposerMenu";
import { WorkbenchComposerHarnessMenu } from "./WorkbenchComposerHarnessMenu";
import {
  MENU_DESCRIPTIONS,
  attachmentDisplayName,
  describeContextWindow,
  labelForVerbosity,
  pickDefaultEffort,
} from "./WorkbenchComposer.utils";
import type { ActiveSessionProps, NewSessionProps, WorkbenchComposerProps } from "./WorkbenchComposer.types";
import { useWorkbenchComposerFloatingMenu } from "./useWorkbenchComposerFloatingMenu";
import { useWorkbenchComposerInputController } from "./useWorkbenchComposerInputController";
import { useWorkbenchComposerModelState } from "./useWorkbenchComposerModelState";

const logoClasses = (base: string, invertInDark?: boolean, invertInLight?: boolean) =>
  [base, invertInDark ? "wb-invert" : "", invertInLight ? "wb-invert-light" : ""].filter(Boolean).join(" ");

export function WorkbenchComposer(props: WorkbenchComposerProps) {
  const {
    variant,
    value,
    setValue,
    placeholder,
    inputDisabled,
    attachments,
    setAttachments,
    onAttachmentError,
    onSend,
    sendDisabled,
    sendDisabledReason,
    onInterrupt,
    isWorking,
    interruptPending,
    sessionIdForAutocomplete,
    workspaceIdForAutocomplete,
    slashCommands,
    recording,
  } = props;

  const newSession = variant === "newSession" ? (props as NewSessionProps) : null;
  const verbosity = props.verbosity ?? "default";
  const canAdjustVerbosity = variant === "newSession" && typeof props.onSetVerbosity === "function";
  const contextWindow =
    variant === "activeSession" ? (props as ActiveSessionProps).contextWindow ?? null : null;
  const showStop = !!onInterrupt && (!!isWorking || !!interruptPending);
  const sendActionDisabled = !!interruptPending || (!showStop && (!!sendDisabled || !!sendDisabledReason));
  const sendActionTitle = interruptPending ? "Stopping..." : showStop ? "Stop" : sendDisabledReason ?? "Send";
  const sendActionLabel = showStop ? (interruptPending ? "Stopping..." : "Stop") : "Send";
  const contextWindowDisplay = useMemo(
    () => (variant === "activeSession" ? describeContextWindow(contextWindow) : null),
    [contextWindow, variant],
  );

  const {
    openMenu,
    setOpenMenu,
    menuStyle,
    rootRef,
    menuRef,
    harnessTriggerRef,
    modelTriggerRef,
    effortTriggerRef,
    verbosityTriggerRef,
  } = useWorkbenchComposerFloatingMenu();
  const {
    clearSelectionBeforeSubmit,
    handleTextareaChange,
    handleTextareaWheelCapture,
    mirrorRef,
    setTextareaElement,
    textareaRef,
  } = useWorkbenchComposerInputController({
    attachments,
    onAttachmentError,
    recording,
    setAttachments,
    setOpenMenu,
    setValue,
    value,
    variant,
  });
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const autocomplete = useComposerAutocomplete({
    sessionId: sessionIdForAutocomplete,
    workspaceId: workspaceIdForAutocomplete ?? null,
    value,
    setValue,
    textareaRef,
    slashCommands,
  });
  const {
    activeModelData,
    currentBase,
    currentEffort,
    currentModelLabel,
    deriveFullModelIdForBase,
    effortOptions,
    harnessControl,
    setActiveModelId,
    showModelEffort,
  } = useWorkbenchComposerModelState({ props, variant, newSession });

  const handleSendAction = useCallback(() => {
    if (showStop) {
      if (interruptPending) {
        return;
      }
      onInterrupt?.();
      return;
    }

    if (variant === "newSession") {
      autocomplete.dismiss();
    }
    clearSelectionBeforeSubmit();
    onSend();
  }, [autocomplete, clearSelectionBeforeSubmit, interruptPending, onInterrupt, onSend, showStop, variant]);

  const verbosityMenu = canAdjustVerbosity ? (
    <div className="wb-menu" role="menu" ref={menuRef} style={menuStyle ?? undefined}>
      <div className="wb-menu-top">
        <MenuTitleRow
          title="Verbosity"
          description={MENU_DESCRIPTIONS.verbosity}
          tooltipId="wb-menu-tooltip-verbosity"
        />
      </div>
      {(["terse", "default", "verbose"] as SessionViewVerbosity[]).map((level) => (
        <button
          key={level}
          type="button"
          className={`wb-menu-item ${verbosity === level ? "wb-menu-item-active" : ""}`}
          onClick={() => {
            props.onSetVerbosity?.(level);
            setOpenMenu(null);
          }}
          role="menuitem"
        >
          {labelForVerbosity(level)}
        </button>
      ))}
    </div>
  ) : null;

  const modelMenu = (
    <div className="wb-menu wb-model-menu" role="menu" ref={menuRef} style={menuStyle ?? undefined}>
      <div className="wb-menu-top">
        <MenuTitleRow title="Model" description={MENU_DESCRIPTIONS.model} tooltipId="wb-menu-tooltip-model" />
        <TextInput
          className="wb-menu-search"
          value={variant === "activeSession" ? "" : ""}
          onChange={() => {}}
          placeholder={activeModelData.loading ? "Loading models…" : "Search models"}
          aria-label="Search models"
          disabled
        />
      </div>

      {activeModelData.catalog.baseIds.length > 0 ? (
        activeModelData.catalog.baseIds.map((b) => (
          <button
            key={b}
            type="button"
            className={`wb-menu-item ${b === currentBase ? "wb-menu-item-active" : ""}`}
            onClick={() => {
              const next = deriveFullModelIdForBase(activeModelData.catalog, b, currentEffort);
              setActiveModelId(next);
              setOpenMenu(null);
            }}
          >
            {activeModelData.catalog.displayNameByBase[b] ?? b}
          </button>
        ))
      ) : (
        <div className="wb-menu-empty">
          <div style={{ marginBottom: 6 }}>{activeModelData.loading ? "Loading models…" : "Enter model id"}</div>
          {!activeModelData.loading ? (
            <TextInput
              className="wb-menu-search"
              value={activeModelData.parsed.full}
              onChange={(e) => setActiveModelId(e.target.value)}
              placeholder="model_id"
              aria-label="Model id"
            />
          ) : null}
        </div>
      )}
    </div>
  );

  const effortMenu = (
    <div className="wb-menu" role="menu" ref={menuRef} style={menuStyle ?? undefined}>
      <div className="wb-menu-top">
        <MenuTitleRow title="Effort" description={MENU_DESCRIPTIONS.effort} tooltipId="wb-menu-tooltip-effort" />
      </div>
      {effortOptions.map((eff) => (
        <button
          key={eff}
          type="button"
          className={`wb-menu-item ${eff === currentEffort ? "wb-menu-item-active" : ""}`}
          onClick={() => {
            const nextFull = deriveFullModelIdForBase(activeModelData.catalog, currentBase, eff);
            setActiveModelId(nextFull);
            setOpenMenu(null);
          }}
        >
          {formatEffortLabel(eff)}
        </button>
      ))}
    </div>
  );


  return (
    <div
      ref={rootRef}
      className={variant === "newSession" ? "wb-composer-card wb-new-composer-card" : "wb-composer wb-active-composer"}
    >
      {contextWindowDisplay && (
        <div
          className="wb-context-window"
          title={contextWindowDisplay.title}
          aria-label={contextWindowDisplay.title}
        >
          {contextWindowDisplay.summary}
        </div>
      )}
      {attachments.length > 0 && (
        <div className="wb-composer-attachments">
          {attachments.map((a, idx) => {
            if (a.kind !== "image" && a.kind !== "image_ref") return null;
            const name = attachmentDisplayName(a.name);
            return (
              <div key={idx} className="wb-attach-thumb" title={name}>
                <MessageAttachmentImage
                  attachment={a}
                  className="wb-attach-thumb-img"
                  alt={name}
                />
                <button
                  type="button"
                  className="wb-attach-thumb-remove"
                  aria-label={`Remove ${name}`}
                  title="Remove attachment"
                  onClick={() => setAttachments((prev) => prev.filter((_, i) => i !== idx))}
                >
                  ×
                </button>
              </div>
            );
          })}
        </div>
      )}

      <Textarea
        ref={setTextareaElement}
        className={
          variant === "newSession"
            ? "wb-composer-textarea wb-new-composer-textarea"
            : "wb-composer-textarea wb-active-textarea"
        }
        placeholder={placeholder}
        value={value}
        onChange={(e) => handleTextareaChange(e.target.value)}
        disabled={!!inputDisabled}
        onWheelCapture={handleTextareaWheelCapture}
        onKeyDown={(e) => {
          if (autocomplete.onKeyDown(e)) return;
          if (shouldSendOnEnter(e)) {
            e.preventDefault();
            handleSendAction();
          }
        }}
        onKeyUp={() => autocomplete.syncFromDom()}
        onClick={() => autocomplete.syncFromDom()}
        onSelect={() => autocomplete.syncFromDom()}
      />
      <div
        ref={mirrorRef}
        className="wb-composer-textarea wb-composer-mirror"
        aria-hidden="true"
      />

      <ComposerAutocompleteMenu
        open={autocomplete.open}
        loading={autocomplete.loading}
        items={autocomplete.items}
        activeIndex={autocomplete.activeIndex}
        onPick={autocomplete.pick}
        onHoverIndex={(i) => autocomplete.setActiveIndex(i)}
        anchorRect={autocomplete.anchorRect}
        anchorInputRect={autocomplete.anchorInputRect}
        inlineFallback={autocomplete.inlineFallback}
      />

      <div className="wb-composer-bottom">
        <div className="wb-switcher-row">
          {/* Harness */}
          <div className="wb-switcher-wrap">
            {variant === "newSession" ? (
              <button
                type="button"
                className="wb-switcher wb-menu-trigger wb-switcher-harness"
                ref={harnessTriggerRef}
                onClick={() => {
                  if (variant !== "newSession") return;
                  setOpenMenu((v) => (v === "harness" ? null : "harness"));
                }}
                aria-haspopup={variant === "newSession" ? "menu" : undefined}
                aria-expanded={openMenu === "harness"}
                aria-label={harnessControl.label}
                title="Agents"
              >
                {harnessControl.logoSrc ? (
                  <img
                    className={logoClasses(
                      "wb-switcher-logo",
                      harnessControl.invertInDark,
                      harnessControl.invertInLight,
                    )}
                    src={harnessControl.logoSrc}
                    alt=""
                  />
                ) : null}
                {harnessControl.label && <span className="wb-switcher-label">{harnessControl.label}</span>}
                <ChevronDown size={14} />
              </button>
            ) : (
              <div
                className="wb-harness-display"
                role="img"
                aria-label={harnessControl.label}
                title="Agents"
              >
                {harnessControl.logoSrc ? (
                  <img
                    className={logoClasses(
                      "wb-switcher-logo",
                      harnessControl.invertInDark,
                      harnessControl.invertInLight,
                    )}
                    src={harnessControl.logoSrc}
                    alt=""
                  />
                ) : null}
              </div>
            )}
            {variant === "newSession" && openMenu === "harness" ? (
              <WorkbenchComposerHarnessMenu
                logoClasses={logoClasses}
                menuRef={menuRef}
                menuStyle={menuStyle}
                newSession={props as NewSessionProps}
                onClose={() => setOpenMenu(null)}
              />
            ) : null}
          </div>

          {/* Model */}
          {showModelEffort && (
            <div className="wb-switcher-wrap">
              <button
                type="button"
                className="wb-switcher wb-menu-trigger"
                ref={modelTriggerRef}
                onClick={() => {
                  if (variant === "newSession" && openMenu !== "model") {
                    const ns = props as NewSessionProps;
                    const providerId = ns.draftHarness?.providerId;
                    if (providerId) {
                      const opts = ns.providerOptions[providerId];
                      if (shouldHydrateProviderModels(providerId, opts, "explicit")) {
                        ns.ensureProviderAuthSummary(providerId, { trigger: "explicit" }).catch(() => {});
                      }
                    }
                  }
                  setOpenMenu((v) => (v === "model" ? null : "model"));
                }}
                aria-haspopup="menu"
                aria-expanded={openMenu === "model"}
                title="Model"
              >
                <span className="wb-switcher-label">{currentModelLabel}</span>
                <ChevronDown size={14} />
              </button>
              {openMenu === "model" && modelMenu}
            </div>
          )}

          {/* Effort (conditional) */}
          {showModelEffort && effortOptions.length > 0 && (
            <div className="wb-switcher-wrap">
              <button
                type="button"
                className="wb-switcher wb-menu-trigger"
                ref={effortTriggerRef}
                onClick={() => setOpenMenu((v) => (v === "effort" ? null : "effort"))}
                aria-haspopup="menu"
                aria-expanded={openMenu === "effort"}
                title="Effort"
              >
                <span className="wb-switcher-label">
                  {(() => {
                    const eff = currentEffort ?? pickDefaultEffort(effortOptions);
                    return eff ? formatEffortLabel(eff) : "Effort";
                  })()}
                </span>
                <ChevronDown size={14} />
              </button>
              {openMenu === "effort" && effortMenu}
            </div>
          )}

        </div>

        <div className="wb-action-row">
          {canAdjustVerbosity ? (
            <>
              <button
                type="button"
                className="wb-icon wb-menu-trigger"
                ref={verbosityTriggerRef}
                onClick={() => setOpenMenu((v) => (v === "verbosity" ? null : "verbosity"))}
                aria-haspopup="menu"
                aria-expanded={openMenu === "verbosity"}
                aria-label="Verbosity"
                title="Verbosity"
              >
                <Ellipsis size={14} />
              </button>
              {openMenu === "verbosity" && verbosityMenu}
            </>
          ) : null}

          <button
            type="button"
            className="wb-icon"
            onClick={() => fileInputRef.current?.click()}
            title="Attach image"
            aria-label="Attach image"
          >
            <Image size={14} />
          </button>
          <input
            ref={fileInputRef}
            type="file"
            accept="image/*"
            multiple
            style={{ display: "none" }}
            onChange={async (e) => {
              const files = Array.from(e.target.files ?? []);
              onAttachmentError?.(null);
              try {
                const next = await imageFilesToMessageAttachments(files);
                setAttachments((prev) => [...prev, ...next]);
              } catch (error: unknown) {
                onAttachmentError?.(errorMessage(error));
              } finally {
                e.target.value = "";
              }
            }}
          />

          {/* First-launch gate: keep dictation wiring in place, but hide composer mic UI for now. */}
          {/*
          <button
            type="button"
            className={`wb-icon ${recording ? "wb-icon-active" : ""}`}
            title={recordDisabledReason ?? (recording ? "Stop recording" : "Record")}
            aria-label="Record"
            disabled={!onToggleRecording}
            onClick={() => onToggleRecording?.()}
          >
            {recording ? <Square size={14} /> : <Mic size={14} />}
          </button>
          */}

          <button
            type="button"
            className="wb-send"
            onClick={handleSendAction}
            disabled={sendActionDisabled}
            title={sendActionTitle}
            aria-label={sendActionLabel}
          >
            {showStop ? <Square size={14} className="wb-stop-icon" /> : <ArrowUp size={14} />}
          </button>
        </div>
      </div>
    </div>
  );
}
