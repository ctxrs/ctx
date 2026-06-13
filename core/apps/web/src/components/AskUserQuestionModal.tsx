import { useMemo, useState } from "react";
import { TextInput } from "./ui/text-input";
import { errorMessage } from "../utils/errorMessage";

type AskUserQuestionOption = { label: string; description?: string };
type AskUserQuestionItem = {
  header: string;
  question: string;
  options: AskUserQuestionOption[];
  multiSelect: boolean;
};

const asRecord = (value: unknown): Record<string, unknown> => {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
};

function normalizeOptions(raw: unknown): AskUserQuestionOption[] {
  if (!Array.isArray(raw)) return [];
  return raw
    .map((o) => {
      if (typeof o === "string") return { label: o };
      if (o && typeof o === "object") {
        const rec = asRecord(o);
        const label = typeof rec.label === "string" ? rec.label : "";
        if (!label.trim()) return null;
        const description = typeof rec.description === "string" ? rec.description : undefined;
        return { label, description };
      }
      return null;
    })
    .filter((o: AskUserQuestionOption | null): o is AskUserQuestionOption => Boolean(o));
}

function normalizeQuestions(input: unknown): AskUserQuestionItem[] {
  const rec = asRecord(input);
  const questions = Array.isArray(rec.questions) ? rec.questions : [];
  return questions
    .map((q, idx: number) => {
      const qRec = asRecord(q);
      const question = typeof qRec.question === "string" ? qRec.question : "";
      if (!question.trim()) return null;
      const header = typeof qRec.header === "string" ? qRec.header : `Question ${idx + 1}`;
      const options = normalizeOptions(qRec.options);
      const multiSelect = Boolean(qRec.multiSelect);
      return { header, question, options, multiSelect };
    })
    .filter((q: AskUserQuestionItem | null): q is AskUserQuestionItem => Boolean(q));
}

export function AskUserQuestionModal({
  open,
  input,
  onSubmit,
  onCancel,
}: {
  open: boolean;
  input: unknown;
  onSubmit: (answers: Record<string, string>) => Promise<void>;
  onCancel: () => Promise<void>;
}) {
  const questions = useMemo(() => normalizeQuestions(input), [input]);
  const [activeIdx, setActiveIdx] = useState(0);
  const [selectedByQuestion, setSelectedByQuestion] = useState<Record<string, Set<string>>>({});
  const [otherByQuestion, setOtherByQuestion] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const allowOther = asRecord(input).request_type !== "permission";
  const active = questions[Math.max(0, Math.min(activeIdx, questions.length - 1))] ?? null;

  const answers = useMemo(() => {
    const out: Record<string, string> = {};
    for (const q of questions) {
      const other = allowOther ? (otherByQuestion[q.question] ?? "").trim() : "";
      if (other) {
        out[q.question] = other;
        continue;
      }
      const selected = selectedByQuestion[q.question];
      if (!selected || selected.size === 0) continue;
      out[q.question] = q.multiSelect ? [...selected].join(", ") : [...selected][0];
    }
    return out;
  }, [allowOther, questions, otherByQuestion, selectedByQuestion]);

  const canSubmit =
    questions.length > 0 &&
    questions.every((q) => typeof answers[q.question] === "string" && answers[q.question].trim().length > 0);

  if (!open) return null;
  if (questions.length === 0) return null;

  return (
    <div className="askq-overlay" role="dialog" aria-modal="true" aria-label="Questions">
      <div className="askq-modal">
        <div className="askq-title">Agent questions</div>

        <div className="askq-tabs" role="tablist" aria-label="Questions">
          {questions.map((q, idx) => {
            const isActive = idx === activeIdx;
            const answered = Boolean(answers[q.question]?.trim());
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
        </div>

        {active ? (
          <div className="askq-body">
            <div className="askq-question">{active.question}</div>
            <div className="askq-options">
              {active.options.map((opt) => {
                const selected = selectedByQuestion[active.question]?.has(opt.label) ?? false;
                return (
                  <label key={opt.label} className={`askq-option ${selected ? "askq-option-selected" : ""}`}>
                    <input
                      type={active.multiSelect ? "checkbox" : "radio"}
                      name={`askq-${active.question}`}
                      checked={selected}
                      disabled={busy}
                      onChange={() => {
                        setError(null);
                        setSelectedByQuestion((prev) => {
                          const next = { ...prev };
                          const set = new Set(next[active.question] ?? []);
                          if (active.multiSelect) {
                            if (set.has(opt.label)) set.delete(opt.label);
                            else set.add(opt.label);
                          } else {
                            set.clear();
                            set.add(opt.label);
                          }
                          next[active.question] = set;
                          return next;
                        });
                      }}
                    />
                    <span className="askq-option-label">{opt.label}</span>
                    {opt.description ? <span className="askq-option-desc">{opt.description}</span> : null}
                  </label>
                );
              })}
            </div>

            {allowOther ? (
              <label className="askq-other">
                <div className="askq-other-label">Other</div>
                <TextInput
                  className="askq-other-input"
                  value={otherByQuestion[active.question] ?? ""}
                  disabled={busy}
                  placeholder="Type something else…"
                  onChange={(e) => {
                    const v = e.target.value;
                    setError(null);
                    setOtherByQuestion((prev) => ({ ...prev, [active.question]: v }));
                  }}
                />
              </label>
            ) : null}

            {error ? <div className="askq-error">{error}</div> : null}
          </div>
        ) : null}

        <div className="askq-actions">
          <button
            type="button"
            className="askq-cancel"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              setError(null);
              try {
                await onCancel();
              } catch (e: unknown) {
                setError(errorMessage(e));
                setBusy(false);
              }
            }}
          >
            Cancel
          </button>
          <button
            type="button"
            className="askq-submit"
            disabled={busy || !canSubmit}
            onClick={async () => {
              if (!canSubmit) {
                setError("Answer all questions or cancel.");
                return;
              }
              setBusy(true);
              setError(null);
              try {
                await onSubmit(answers);
              } catch (e: unknown) {
                setError(errorMessage(e));
                setBusy(false);
              }
            }}
          >
            Submit
          </button>
        </div>
      </div>
    </div>
  );
}
