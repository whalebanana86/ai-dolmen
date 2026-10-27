import { useEffect, useState } from "react";
import { HistoryFilter, HistoryRun, SessionInfo, errorMessage, history } from "./api";
import { PanelShell } from "./DetailPanel";
import { useT } from "./settings";

interface Props {
  session: SessionInfo;
  /** Whether this connection records its queries. */
  recording: boolean;
  onInsert: (sql: string) => void;
  onRun: (sql: string) => void;
  onClose: () => void;
  width: number;
  onResize: (width: number) => void;
  onResetWidth: () => void;
}

const PAGE = 100;

/** The statements run before, newest first, to search, star, reuse or delete. */
export function HistoryPanel({ session, recording, onInsert, onRun, onClose, width, onResize, onResetWidth }: Props) {
  const t = useT();
  const [scope, setScope] = useState<"this" | "all">("this");
  const [text, setText] = useState("");
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<"all" | "ok" | "error">("all");
  const [starredOnly, setStarredOnly] = useState(false);
  const [runs, setRuns] = useState<HistoryRun[]>([]);
  const [more, setMore] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<unknown>(null);

  // Search once typing pauses.
  useEffect(() => {
    const timer = setTimeout(() => setQuery(text), 250);
    return () => clearTimeout(timer);
  }, [text]);

  const filter = (before: number | null): HistoryFilter => ({
    ...(scope === "this"
      ? session.savedId
        ? { connectionId: session.savedId }
        : { connection: session.label }
      : {}),
    text: query || null,
    status: status === "all" ? null : status,
    starredOnly,
    before,
    limit: PAGE,
  });

  async function load(before: number | null) {
    setLoading(true);
    setError(null);
    try {
      const page = await history.list(filter(before));
      setRuns((current) => (before === null ? page : [...current, ...page]));
      setMore(page.length === PAGE);
    } catch (err) {
      setError(err);
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    load(null);
  }, [scope, query, status, starredOnly]);

  async function act(action: () => Promise<void>, change: (runs: HistoryRun[]) => HistoryRun[]) {
    try {
      await action();
      setRuns(change);
    } catch (err) {
      setError(err);
    }
  }

  const filtered = query !== "" || status !== "all" || starredOnly;
  let lastDay = "";

  return (
    <PanelShell
      label={t.history.title}
      width={width}
      onResize={onResize}
      onResetWidth={onResetWidth}
      onClose={onClose}
      header={
        <>
          <strong>{t.history.title}</strong>
          <span className="spacer" />
        </>
      }
    >
      <div className="history-filters">
        <input
          type="search"
          value={text}
          placeholder={t.history.search}
          onChange={(e) => setText(e.target.value)}
          autoFocus
          aria-label={t.history.search}
        />
        <div className="history-filter-row">
          <div className="tabs" role="tablist">
            {(["this", "all"] as const).map((value) => (
              <button
                key={value}
                role="tab"
                aria-selected={scope === value}
                className={scope === value ? "selected" : undefined}
                onClick={() => setScope(value)}
              >
                {value === "this" ? t.history.thisConnection : t.history.allConnections}
              </button>
            ))}
          </div>
          <div className="tabs" role="tablist">
            {(["all", "ok", "error"] as const).map((value) => (
              <button
                key={value}
                role="tab"
                aria-selected={status === value}
                className={status === value ? "selected" : undefined}
                onClick={() => setStatus(value)}
              >
                {t.history.statuses[value]}
              </button>
            ))}
          </div>
          <label className="checkbox">
            <input type="checkbox" checked={starredOnly} onChange={(e) => setStarredOnly(e.target.checked)} />
            {t.history.starredOnly}
          </label>
        </div>
        {!recording && scope === "this" && <p className="muted">{t.history.notRecorded}</p>}
      </div>

      <div className="detail-body history-list">
        {error !== null && <p className="error-text">{errorMessage(error, t)}</p>}
        {!loading && runs.length === 0 && error === null && (
          <p className="muted">{filtered ? t.history.noMatches : t.history.empty}</p>
        )}
        {runs.map((run) => {
          const day = dayLabel(run.startedAt, t.history);
          const header = day !== lastDay ? <h4 className="history-day">{day}</h4> : null;
          lastDay = day;
          return (
            <div key={run.id}>
              {header}
              <article className={`history-run ${run.status}`}>
                <header>
                  <button
                    className={`icon-button star${run.starred ? " starred" : ""}`}
                    onClick={() =>
                      act(
                        () => history.star(run.id, !run.starred),
                        (list) => list.map((r) => (r.id === run.id ? { ...r, starred: !run.starred } : r)),
                      )
                    }
                    title={run.starred ? t.history.unstar : t.history.star}
                    aria-label={run.starred ? t.history.unstar : t.history.star}
                    aria-pressed={run.starred}
                  >
                    {run.starred ? "★" : "☆"}
                  </button>
                  <span className="muted">
                    {new Date(run.startedAt).toLocaleTimeString()} · {run.durationMs.toLocaleString()} ms
                    {run.status === "ok" && run.rows !== null && ` · ${t.history.rows(run.rows)}`}
                    {run.status === "cancelled" && ` · ${t.history.cancelled}`}
                  </span>
                  {scope === "all" && <span className="history-connection">{run.connection}</span>}
                  {t.history.sources[run.source] && (
                    <span className="detail-kind">{t.history.sources[run.source]}</span>
                  )}
                  <span className="spacer" />
                  <div className="history-actions">
                    <button onClick={() => onInsert(run.sql)}>{t.history.insert}</button>
                    {run.source === "editor" && <button onClick={() => onRun(run.sql)}>{t.history.runAgain}</button>}
                    <button onClick={() => navigator.clipboard.writeText(run.sql)}>{t.history.copy}</button>
                    <button
                      className="icon-button"
                      onClick={() =>
                        act(
                          () => history.delete([run.id]),
                          (list) => list.filter((r) => r.id !== run.id),
                        )
                      }
                      title={t.history.remove}
                      aria-label={t.history.remove}
                    >
                      ✕
                    </button>
                  </div>
                </header>
                <pre className="history-sql" onDoubleClick={() => onInsert(run.sql)}>
                  {run.sql}
                </pre>
                {run.error && run.status === "error" && <p className="error-text history-error">{run.error}</p>}
              </article>
            </div>
          );
        })}
        {more && (
          <div className="actions">
            <button onClick={() => load(runs[runs.length - 1].id)} disabled={loading}>
              {t.history.more}
            </button>
          </div>
        )}
      </div>
    </PanelShell>
  );
}

function dayLabel(time: number, labels: { today: string; yesterday: string }) {
  const day = new Date(time);
  const today = new Date();
  const yesterday = new Date(today.getFullYear(), today.getMonth(), today.getDate() - 1);
  if (day.toDateString() === today.toDateString()) return labels.today;
  if (day.toDateString() === yesterday.toDateString()) return labels.yesterday;
  return day.toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric", weekday: "short" });
}
