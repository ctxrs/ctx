import { TextInput } from "./ui/text-input";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  buildAskUserQuestionAnswersFromState,
  DEFAULT_OTHER_LABEL,
  deriveAskUserQuestionSelectionState,
  normalizeAskUserQuestions,
  type AskUserQuestionItem,
  type AskUserQuestionOption,
} from "./askUserQuestionShared";
import { errorMessage } from "../utils/errorMessage";

export function AskUserQuestionCard({
  input,
  answers,
  outcome,
  readOnly,
  active,
  onSubmit,
  onCancel,
}: {
  input: unknown;
  answers?: Record<string, string>;
  outcome?: "submitted" | "cancelled";
  readOnly: boolean;
  active: boolean;
  onSubmit?: (answers: Record<string, string>) => Promise<void>;
  onCancel?: () => Promise<void>;
}) {
  const questions = useMemo(() => normalizeAskUserQuestions(input), [input]);
  const initialSelections = useMemo(
    () => deriveAskUserQuestionSelectionState(questions, answers),
    [questions, answers],
  );
  const [activeIdx, setActiveIdx] = useState(0);
  const [selectedByQuestion, setSelectedByQuestion] = useState<Record<string, Set<string>>>(
    () => initialSelections.selectedByQuestion,
  );
  const [otherByQuestion, setOtherByQuestion] = useState<Record<string, string>>(
    () => initialSelections.otherByQuestion,
  );
  const [cursorIndexByQuestion, setCursorIndexByQuestion] = useState<Record<string, number>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const cardRef = useRef<HTMLDivElement | null>(null);
  const otherInputRefs = useRef<Record<string, HTMLInputElement | null>>({});

  useEffect(() => {
    if (!answers && !readOnly) return;
    setSelectedByQuestion(initialSelections.selectedByQuestion);
    setOtherByQuestion(initialSelections.otherByQuestion);
  }, [answers, initialSelections, readOnly]);

  useEffect(() => {
    const maxIndex = Math.max(0, questions.length);
    setActiveIdx((prev) => Math.min(prev, maxIndex));
  }, [questions.length]);

  useEffect(() => {
    if (!active || readOnly || busy) return;
    const node = cardRef.current;
    if (!node) return;
    if (document.activeElement && node.contains(document.activeElement)) return;
    node.focus();
  }, [active, readOnly, busy]);

  const draftAnswers = useMemo(
    () => buildAskUserQuestionAnswersFromState(questions, selectedByQuestion, otherByQuestion),
    [questions, selectedByQuestion, otherByQuestion],
  );
  const effectiveAnswers = answers ?? draftAnswers;
  const canSubmit =
    questions.length > 0 &&
    questions.every((q) => typeof draftAnswers[q.question] === "string" && draftAnswers[q.question].trim());
  const submitTabIndex = questions.length;
  const maxTabIndex = submitTabIndex;
  const isSubmitTab = activeIdx >= submitTabIndex;
  const activeQuestion = !isSubmitTab ? questions[Math.max(0, Math.min(activeIdx, questions.length - 1))] : null;
  const submitLabel = outcome === "cancelled" ? "Cancelled" : readOnly ? "Submitted" : "Submit";

  const displayOptions = useMemo(() => {
    if (!activeQuestion) return [];
    const otherLabel =
      activeQuestion.otherLabel ??
      ((otherByQuestion[activeQuestion.question] ?? "").trim() ? DEFAULT_OTHER_LABEL : undefined);
    if (!otherLabel) return activeQuestion.options;
    return [...activeQuestion.options, { label: otherLabel, isOther: true }];
  }, [activeQuestion, otherByQuestion]);

  if (questions.length === 0) return null;

  const cursorIndex = (() => {
    if (!activeQuestion) return 0;
    const existing = cursorIndexByQuestion[activeQuestion.question];
    if (typeof existing === "number") return existing;
    const selected = selectedByQuestion[activeQuestion.question];
    if (selected && selected.size > 0) {
      const selectedLabels = new Set(selected);
      const matchIndex = displayOptions.findIndex((opt) => selectedLabels.has(opt.label));
      if (matchIndex >= 0) return matchIndex;
    }
    return 0;
  })();

  const updateCursorIndex = (idx: number) => {
    if (!activeQuestion) return;
    setCursorIndexByQuestion((prev) => ({ ...prev, [activeQuestion.question]: idx }));
  };

  const selectOption = (opt: AskUserQuestionOption, idx: number) => {
    if (!activeQuestion || readOnly || busy) return;
    setError(null);
    updateCursorIndex(idx);
    const otherLabel = activeQuestion.otherLabel ?? (opt.isOther ? opt.label : undefined);
    setSelectedByQuestion((prev) => {
      const next = { ...prev };
      const set = new Set(next[activeQuestion.question] ?? []);
      if (activeQuestion.multiSelect) {
        if (opt.isOther) {
          if (set.has(opt.label)) {
            set.delete(opt.label);
          } else {
            set.add(opt.label);
          }
        } else if (set.has(opt.label)) {
          set.delete(opt.label);
        } else {
          set.add(opt.label);
        }
      } else {
        set.clear();
        set.add(opt.label);
      }
      if (!opt.isOther && otherLabel) {
        set.delete(otherLabel);
      }
      next[activeQuestion.question] = set;
      return next;
    });
    if (!opt.isOther && otherLabel) {
      setOtherByQuestion((prev) => ({ ...prev, [activeQuestion.question]: "" }));
    }
    if (opt.isOther) {
      if (activeQuestion.multiSelect && selectedByQuestion[activeQuestion.question]?.has(opt.label)) {
        setOtherByQuestion((prev) => ({ ...prev, [activeQuestion.question]: "" }));
      }
      const ref = otherInputRefs.current[activeQuestion.question];
      if (ref) {
        ref.focus();
        ref.select();
      }
    }
  };

  const handleTabNav = (delta: number) => {
    const next = activeIdx + delta;
    if (next < 0) setActiveIdx(maxTabIndex);
    else if (next > maxTabIndex) setActiveIdx(0);
    else setActiveIdx(next);
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    const target = event.target as HTMLElement | null;
    if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) {
      if (event.key === "Escape" && !readOnly && onCancel) {
        event.preventDefault();
        void onCancel();
      }
      return;
    }

    switch (event.key) {
      case "Tab":
        event.preventDefault();
        handleTabNav(event.shiftKey ? -1 : 1);
        return;
      case "ArrowLeft":
        event.preventDefault();
        handleTabNav(-1);
        return;
      case "ArrowRight":
        event.preventDefault();
        handleTabNav(1);
        return;
      case "ArrowUp":
        if (isSubmitTab) return;
        if (displayOptions.length === 0) return;
        event.preventDefault();
        updateCursorIndex(Math.max(0, cursorIndex - 1));
        return;
      case "ArrowDown":
        if (isSubmitTab) return;
        if (displayOptions.length === 0) return;
        event.preventDefault();
        updateCursorIndex(Math.min(displayOptions.length - 1, cursorIndex + 1));
        return;
      case "Enter":
        event.preventDefault();
        if (isSubmitTab) {
          if (readOnly || busy || !onSubmit || !canSubmit) return;
          setBusy(true);
          setError(null);
          void onSubmit(draftAnswers)
            .catch((e: unknown) => setError(errorMessage(e)))
            .finally(() => setBusy(false));
          return;
        }
        if (displayOptions.length === 0) return;
        if (displayOptions[cursorIndex]) {
          selectOption(displayOptions[cursorIndex], cursorIndex);
        }
        return;
      case "Escape":
        if (readOnly || !onCancel) return;
        event.preventDefault();
        void onCancel();
        return;
      default:
        break;
    }

    if (!isSubmitTab && event.key >= "1" && event.key <= "9") {
      const idx = Number(event.key) - 1;
      if (idx >= 0 && idx < displayOptions.length) {
        event.preventDefault();
        selectOption(displayOptions[idx], idx);
      }
    }
  };

  return (
    <div
      className={`askq-card${readOnly ? " askq-card-readonly" : ""}`}
      ref={cardRef}
      tabIndex={0}
      role="group"
      aria-label="Questions"
      onKeyDown={handleKeyDown}
    >
      <div className="askq-tabs" role="tablist" aria-label="Questions">
        {questions.map((q, idx) => {
          const isActive = idx === activeIdx;
          const answered = Boolean(effectiveAnswers[q.question]?.trim());
          return (
            <button
              key={`${q.header}-${idx}`}
              type="button"
              role="tab"
              aria-selected={isActive}
              className={`askq-tab ${isActive ? "askq-tab-active" : ""}`}
              onClick={() => setActiveIdx(idx)}
              disabled={busy}
            >
              {q.header}
              {answered ? <span className="askq-tab-dot" aria-hidden="true" /> : null}
            </button>
          );
        })}
        <button
          type="button"
          role="tab"
          aria-selected={isSubmitTab}
          className={`askq-tab ${isSubmitTab ? "askq-tab-active" : ""}`}
          onClick={() => setActiveIdx(submitTabIndex)}
          disabled={busy}
        >
          {submitLabel}
        </button>
      </div>

      <div className="askq-panel-viewport">
        {isSubmitTab ? (
          <div className="askq-submit-panel">
            <div className="askq-submit-title">
              {readOnly
                ? outcome === "cancelled"
                  ? "Submission cancelled."
                  : "Submitted answers."
                : "Review answers before submitting."}
            </div>
            <div className="askq-summary">
              {questions.map((q) => {
                const answer = String(effectiveAnswers[q.question] ?? "").trim();
                return (
                  <div key={q.question} className="askq-summary-row">
                    <div className="askq-summary-question">{q.question}</div>
                    <div className={`askq-summary-answer${answer ? "" : " askq-summary-missing"}`}>
                      {answer || "Missing"}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        ) : activeQuestion ? (
          <div className="askq-body">
            <div className="askq-question">{activeQuestion.question}</div>
            <div className="askq-options" role="listbox" aria-label={activeQuestion.header}>
              {displayOptions.map((opt, idx) => {
                const selected = selectedByQuestion[activeQuestion.question]?.has(opt.label) ?? false;
                const isActive = idx === cursorIndex;
                return (
                  <div
                    key={`${opt.label}-${idx}`}
                    className={`askq-option${selected ? " askq-option-selected" : ""}${isActive ? " askq-option-active" : ""}${opt.isOther ? " askq-option-other" : ""}`}
                    role="option"
                    aria-selected={selected}
                    onClick={() => selectOption(opt, idx)}
                  >
                    <span className="askq-option-marker" aria-hidden="true">
                      {isActive ? ">" : ""}
                    </span>
                    <span className="askq-option-index">{idx + 1}.</span>
                    <span className="askq-option-label">{opt.label}</span>
                    {opt.description ? <span className="askq-option-desc">{opt.description}</span> : null}
                  </div>
                );
              })}
            </div>
            {(() => {
              const otherValue = otherByQuestion[activeQuestion.question] ?? "";
              const otherLabel =
                activeQuestion.otherLabel ??
                (otherValue.trim() ? DEFAULT_OTHER_LABEL : undefined);
              if (!otherLabel) return null;
              const isSelected =
                selectedByQuestion[activeQuestion.question]?.has(otherLabel) ?? otherValue.trim().length > 0;
              if (!isSelected && readOnly) return null;
              return (
                <div className="askq-other">
                  <div className="askq-other-label">{otherLabel}</div>
                  <TextInput
                    ref={(node) => {
                      otherInputRefs.current[activeQuestion.question] = node;
                    }}
                    className="askq-other-input"
                    value={otherValue}
                    disabled={readOnly || busy}
                    placeholder="Type something"
                    onChange={(e) => {
                      if (readOnly || busy) return;
                      const v = e.target.value;
                      setError(null);
                      setOtherByQuestion((prev) => ({ ...prev, [activeQuestion.question]: v }));
                      setSelectedByQuestion((prev) => {
                        const next = { ...prev };
                        const set = new Set(next[activeQuestion.question] ?? []);
                        set.add(otherLabel);
                        next[activeQuestion.question] = set;
                        return next;
                      });
                    }}
                  />
                </div>
              );
            })()}
          </div>
        ) : null}
      </div>

      <div className="askq-status-slot" aria-live="polite">
        {error ? <div className="askq-error">{error}</div> : null}
      </div>

      {!readOnly ? (
        <div className="askq-actions">
          <button
            type="button"
            className="askq-cancel"
            disabled={busy}
            onClick={() => {
              if (!onCancel) return;
              setBusy(true);
              setError(null);
              void onCancel()
                .catch((e: unknown) => setError(errorMessage(e)))
                .finally(() => setBusy(false));
            }}
          >
            Cancel
          </button>
          <button
            type="button"
            className="askq-submit"
            disabled={busy || !canSubmit || !onSubmit}
            onClick={() => {
              if (!onSubmit || !canSubmit) {
                setError("Answer all questions or cancel.");
                return;
              }
              setBusy(true);
              setError(null);
              void onSubmit(draftAnswers)
                .catch((e: unknown) => setError(errorMessage(e)))
                .finally(() => setBusy(false));
            }}
          >
            Submit
          </button>
        </div>
      ) : (
        <div className="askq-actions askq-actions-readonly">
          {outcome === "cancelled" ? "Cancelled." : "Submitted answers are read-only."}
        </div>
      )}

      <div className="askq-hint">
        {!readOnly
          ? "Use left/right or Tab to change tabs. Use arrows or 1-9 to select. Enter selects the highlighted option."
          : "Read-only answers. Use left/right to switch tabs."}
      </div>
    </div>
  );
}
