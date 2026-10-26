import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import { AiAnswer, AiTask, ai, errorMessage, isAppError } from "./api";
import { LANGUAGE_NAMES, useSettings, useT } from "./settings";

/** Where a failed statement was in the editor, for replacing it with a fix. */
export interface FixTarget {
  statement: string;
  start: number;
  end: number;
}

export interface Conversation {
  id: number;
  task: AiTask;
  target: FixTarget | null;
  /** The answer so far. */
  text: string;
  status: "asking" | "done" | "failed" | "stopped";
  answer: AiAnswer | null;
  error: unknown;
}

/** Asks about one task at a time; asking again stops the answer before. */
export function useAi(sessionId: number) {
  const { settings } = useSettings();
  const [conversation, setConversation] = useState<Conversation | null>(null);
  const nextId = useRef(1);
  const asking = useRef<number | null>(null);

  const ask = useCallback(
    (task: AiTask, target: FixTarget | null = null) => {
      if (asking.current !== null) ai.cancel(asking.current).catch(() => {});
      const id = nextId.current++;
      asking.current = id;
      setConversation({ id, task, target, text: "", status: "asking", answer: null, error: null });
      const update = (change: (c: Conversation) => Partial<Conversation>) =>
        setConversation((c) => (c && c.id === id ? { ...c, ...change(c) } : c));
      const question = {
        id,
        sessionId,
        model: settings.aiModel,
        language: LANGUAGE_NAMES[settings.locale],
        task,
      };
      ai.ask(question, (text) => update((c) => ({ text: c.text + text })))
        .then((answer) => update(() => ({ status: "done", answer })))
        .catch((error) =>
          update(() =>
            isAppError(error) && error.kind === "ai" && error.reason === "cancelled"
              ? { status: "stopped" }
              : { status: "failed", error },
          ),
        )
        .finally(() => {
          if (asking.current === id) asking.current = null;
        });
    },
    [sessionId, settings.aiModel, settings.locale],
  );

  const stop = useCallback(() => {
    if (asking.current !== null) ai.cancel(asking.current).catch(() => {});
  }, []);

  const clear = useCallback(() => {
    stop();
    setConversation(null);
  }, [stop]);

  // Stop the answer when the panel goes away, e.g. on disconnect.
  useEffect(() => stop, [stop]);

  return { conversation, ask, stop, clear };
}

interface Props {
  conversation: Conversation | null;
  /** Whether the prompt is showing. */
  prompting: boolean;
  onGenerate: (request: string) => void;
  onExplain: () => void;
  onStop: () => void;
  onClose: () => void;
  onInsert: (sql: string) => void;
  onReplace: (sql: string, target: FixTarget) => void;
  onOpenSettings: () => void;
}

/**
 * Asks Claude to write SQL from a request, explain SQL or fix a failed
 * statement, and shows the answer as it arrives. SQL in the answer goes into
 * the editor only when asked, and is never run from here.
 */
export function AiPanel({
  conversation,
  prompting,
  onGenerate,
  onExplain,
  onStop,
  onClose,
  onInsert,
  onReplace,
  onOpenSettings,
}: Props) {
  const t = useT();
  const [request, setRequest] = useState("");
  const asking = conversation?.status === "asking";
  const error = conversation?.status === "failed" ? conversation.error : null;
  const noKey = isAppError(error) && error.kind === "ai" && ["noApiKey", "badApiKey"].includes(error.reason);

  return (
    <section className="ai-panel" aria-label="AI">
      <header className="ai-header">
        {prompting ? (
          <form
            className="ai-prompt"
            onSubmit={(e) => {
              e.preventDefault();
              if (request.trim()) onGenerate(request.trim());
            }}
          >
            <input
              autoFocus
              value={request}
              placeholder={t.ai.placeholder}
              onChange={(e) => setRequest(e.target.value)}
              onKeyDown={(e) => e.key === "Escape" && onClose()}
              aria-label={t.ai.placeholder}
            />
            <button type="submit" className="primary" disabled={!request.trim()}>
              {t.ai.generate}
            </button>
            <button type="button" onClick={onExplain} title={t.ai.explainTitle}>
              {t.ai.explain}
            </button>
          </form>
        ) : (
          <strong className="ai-title">{conversation && askedLabel(conversation.task, t)}</strong>
        )}
        {asking && <button onClick={onStop}>{t.ai.stop}</button>}
        <button onClick={onClose} title={t.common.close}>
          ✕
        </button>
      </header>

      {conversation && (
        <div className="ai-answer">
          {prompting && <p className="ai-title">{askedLabel(conversation.task, t)}</p>}
          {asking && !conversation.text && <p className="muted">{t.ai.thinking}</p>}
          {splitAnswer(conversation.text).map((segment, i) =>
            segment.kind === "text" ? (
              <Prose key={i} text={segment.text} />
            ) : (
              <div key={i} className="ai-sql">
                <pre>{segment.sql}</pre>
                {segment.complete && segment.sql.trim() && (
                  <div className="actions">
                    {conversation.target && (
                      <button className="primary" onClick={() => onReplace(segment.sql, conversation.target!)}>
                        {t.ai.replace}
                      </button>
                    )}
                    <button className={conversation.target ? undefined : "primary"} onClick={() => onInsert(segment.sql)}>
                      {t.ai.insert}
                    </button>
                  </div>
                )}
              </div>
            ),
          )}
          {conversation.status === "stopped" && <p className="muted">{t.errors.ai.cancelled}</p>}
          {conversation.answer?.stopReason === "max_tokens" && <p className="error-text">{t.ai.cutOff}</p>}
          {error !== null && (
            <div className="error-text">
              <p>{errorMessage(error, t)}</p>
              {noKey && <button onClick={onOpenSettings}>{t.ai.openSettings}</button>}
            </div>
          )}
          {conversation.status === "done" && <p className="muted ai-note">{t.ai.neverRuns}</p>}
        </div>
      )}
    </section>
  );
}

function askedLabel(task: AiTask, t: ReturnType<typeof useT>) {
  switch (task.kind) {
    case "generate":
      return t.ai.asked.generate(task.request);
    case "explain":
      return t.ai.asked.explain;
    case "fix":
      return t.ai.asked.fix;
  }
}

type Segment = { kind: "text"; text: string } | { kind: "sql"; sql: string; complete: boolean };

/** Splits an answer, possibly still arriving, into prose and fenced code blocks. */
export function splitAnswer(text: string): Segment[] {
  const segments: Segment[] = [];
  const pushText = (part: string) => {
    if (part.trim()) segments.push({ kind: "text", text: part.trim() });
  };
  let at = 0;
  while (at < text.length) {
    const open = text.indexOf("```", at);
    if (open < 0) {
      pushText(text.slice(at));
      break;
    }
    pushText(text.slice(at, open));
    const lineEnd = text.indexOf("\n", open);
    if (lineEnd < 0) {
      // The fence's first line is still arriving.
      segments.push({ kind: "sql", sql: "", complete: false });
      break;
    }
    const close = text.indexOf("```", lineEnd + 1);
    if (close < 0) {
      segments.push({ kind: "sql", sql: text.slice(lineEnd + 1), complete: false });
      break;
    }
    segments.push({ kind: "sql", sql: text.slice(lineEnd + 1, close).replace(/\n$/, ""), complete: true });
    at = close + 3;
  }
  return segments;
}

/** Prose with `code` and **bold** shown as such; everything else as plain text. */
function Prose({ text }: { text: string }) {
  return (
    <p className="ai-prose">
      {text.split(/(`[^`\n]+`|\*\*[^*\n]+\*\*)/).map((part, i) =>
        part.startsWith("`") && part.endsWith("`") && part.length > 1 ? (
          <code key={i}>{part.slice(1, -1)}</code>
        ) : part.startsWith("**") && part.endsWith("**") && part.length > 4 ? (
          <strong key={i}>{part.slice(2, -2)}</strong>
        ) : (
          <Fragment key={i}>{part}</Fragment>
        ),
      )}
    </p>
  );
}
