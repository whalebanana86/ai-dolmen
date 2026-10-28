import { ReactNode, useState } from "react";
import { Environment, StatementResult, errorMessage, isAppError, pg } from "./api";
import { EnvironmentBadge } from "./ConnectionForm";
import type { Messages } from "./i18n/en";
import { ResultGrid } from "./ResultGrid";
import { NO_CHANGES, PendingRows, addRow, changeCount, savedRows, toChanges } from "./rowEdits";
import { useT } from "./settings";
import { statementTable } from "./sqlIdent";

/** Where changes to rows are saved. */
export interface SaveTarget {
  /** The session the statement ran in, whose transaction the changes join. */
  sessionId: number;
  readOnly: boolean;
  /** The connection's name and address, to say which database a save changes. */
  label: string;
  address: string;
  environment: Environment | null;
}

interface Props {
  index: number;
  result: StatementResult;
  /** The statement that gave the result. */
  sql: string;
  /** Changes to its rows not saved yet. */
  pending: PendingRows;
  /** Where changes are saved; null when they can't be. */
  target: SaveTarget | null;
  onChange: (change: { result?: StatementResult; pending?: PendingRows }) => void;
  /** Selects the statement in the editor. */
  onReveal: () => void;
  /** Saves the statement's rows to a file. */
  onExport: () => void;
}

type Status =
  | { kind: "saving" }
  | { kind: "saved"; count: number; committed: boolean }
  /** Nothing was saved: `row` (a grid row) changed elsewhere, or failed with `error`. */
  | { kind: "stale"; row: number }
  | { kind: "failed"; row: number | null; error: unknown };

export function ResultView({ index, result, sql, pending, target, onChange, onReveal, onExport }: Props) {
  const t = useT();
  const table = statementTable(sql);
  const count = resultCount(result, t);
  const [notice, setNotice] = useState<string | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [preview, setPreview] = useState<string[] | null>(null);
  const [confirming, setConfirming] = useState<string[] | null>(null);

  if (result.kind !== "rows") {
    return (
      <div className="result">
        <ResultTitle index={index} table={table} count={count} />
        <StatementLine sql={sql} onReveal={onReveal} />
      </div>
    );
  }

  // Narrowed, for the functions below.
  const shown = result;
  const rows = shown.rows;
  const source = shown.source?.kind === "table" ? shown.source : null;
  const changes = changeCount(pending);
  const saving = status?.kind === "saving";
  const locked = !target || target.readOnly || saving;

  function changePending(next: PendingRows) {
    setNotice(null);
    setPreview(null);
    if (status?.kind !== "saving") setStatus(null);
    onChange({ pending: next });
  }

  /** Says why a column, or the whole result (null), can't be changed. */
  function explain(column: number | null) {
    if (!shown.source) return setNotice(t.rows.cantEdit);
    if (shown.source.kind === "readOnly") return setNotice(t.rows.readOnly[shown.source.reason]);
    if (!target || target.readOnly) return setNotice(t.rows.locked);
    if (column === null) return;
    const from = shown.source.columns[column];
    setNotice(!from ? t.rows.computed : from.generated ? t.rows.generated(from.name) : t.rows.identity(from.name));
  }

  async function showPreview() {
    if (!source) return;
    if (preview) return setPreview(null);
    try {
      setPreview(await pg.previewRows(toChanges(source, rows, pending).request));
    } catch (error) {
      setStatus({ kind: "failed", row: null, error });
    }
  }

  async function save(confirmed = false) {
    if (!source || !target || changes === 0 || saving) return;
    if (target.readOnly) return setNotice(t.rows.locked);
    const { request, rowOf } = toChanges(source, rows, pending);
    setConfirming(null);
    try {
      // Production asks first, showing what will run.
      if (target.environment === "production" && !confirmed) {
        setConfirming(await pg.previewRows(request));
        return;
      }
      setStatus({ kind: "saving" });
      const saved = await pg.saveRows(target.sessionId, request);
      if (saved.kind === "stale") return setStatus({ kind: "stale", row: rowOf[saved.change] });
      if (saved.kind === "failed") {
        return setStatus({ kind: "failed", row: rowOf[saved.change], error: saved.error });
      }
      const now = savedRows(source, rows, request.changes, rowOf, saved.rows);
      const inserted = pending.added.length;
      onChange({
        result: { ...shown, rows: now, totalRows: shown.totalRows + inserted - pending.deleted.length },
        pending: NO_CHANGES,
      });
      setPreview(null);
      setStatus({ kind: "saved", count: request.changes.length, committed: saved.committed });
    } catch (error) {
      setStatus({ kind: "failed", row: null, error });
    }
  }

  const problemRow = status?.kind === "stale" || status?.kind === "failed" ? status.row : null;

  return (
    <div className="result">
      <ResultTitle index={index} table={table} count={count}>
        {result.columns.length > 0 && (
          <button className="result-export" onClick={onExport} title={t.results.exportTitle}>
            {t.results.export}
          </button>
        )}
        {source && (
          <button
            className="result-export"
            onClick={() => (locked ? explain(null) : changePending(addRow(pending, result.columns.length)))}
            title={t.rows.addRowTitle}
          >
            {t.rows.addRow}
          </button>
        )}
      </ResultTitle>
      <StatementLine sql={sql} onReveal={onReveal} />
      {notice && <p className="rows-notice">{notice}</p>}
      {changes > 0 && (
        <div className="rows-bar">
          <span>{t.rows.unsaved(changes)}</span>
          <span className="spacer" />
          <button onClick={showPreview} disabled={saving}>
            {preview ? t.rows.hideSql : t.rows.showSql}
          </button>
          <button onClick={() => changePending(NO_CHANGES)} disabled={saving}>
            {t.rows.discard}
          </button>
          <button className="primary" onClick={() => save()} disabled={saving} title={t.rows.saveTitle}>
            {saving ? t.rows.saving : t.rows.save}
          </button>
        </div>
      )}
      {preview && <pre className="rows-sql">{preview.join("\n")}</pre>}
      {status && status.kind !== "saving" && <SaveStatus status={status} />}
      <ResultGrid
        columns={result.columns}
        rows={rows}
        columnNotes={source?.columns.map((column) => column && columnNote(column, t))}
        editing={
          source
            ? {
                pending,
                editable: source.columns.map((c) => !!c && !c.generated && !c.identityAlways),
                computed: source.columns.map((c) => !c),
                locked,
                onChange: changePending,
                onSave: () => save(),
              }
            : undefined
        }
        onCantEdit={explain}
        problemRow={problemRow}
      />
      {confirming && target && (
        <ConfirmSave
          statements={confirming}
          target={target}
          onSave={() => save(true)}
          onCancel={() => setConfirming(null)}
        />
      )}
    </div>
  );
}

function ResultTitle({
  index,
  table,
  count,
  children,
}: {
  index: number;
  table: string | null;
  count: string;
  children?: ReactNode;
}) {
  const t = useT();
  return (
    <p className="result-title">
      {t.results.statement(index + 1)}
      {table && (
        <>
          {" · "}
          <strong className="result-table">{table}</strong>
        </>
      )}
      {" · "}
      {count}
      {children}
    </p>
  );
}

/** "integer · NOT NULL", for a column's header. */
function columnNote(column: { dataType: string; nullable: boolean }, t: Messages) {
  return column.nullable ? column.dataType : `${column.dataType} · ${t.rows.notNull}`;
}

function SaveStatus({ status }: { status: Exclude<Status, { kind: "saving" }> }) {
  const t = useT();
  if (status.kind === "saved") {
    return (
      <p className={status.committed ? "rows-notice" : "rows-notice warning"}>
        {status.committed ? t.rows.saved(status.count) : t.rows.savedInTransaction(status.count)}
      </p>
    );
  }
  if (status.kind === "stale") {
    return (
      <div className="error-box rows-error">
        <p>
          <strong>{t.rows.stale(status.row + 1)}</strong>
        </p>
      </div>
    );
  }
  const db = isAppError(status.error) && status.error.kind === "database" ? status.error : null;
  return (
    <div className="error-box rows-error">
      {status.row !== null && <p>{t.rows.failed(status.row + 1)}</p>}
      <p>
        <strong>{errorMessage(status.error, t)}</strong>
      </p>
      {db && <p className="muted">SQLSTATE {db.code}</p>}
      {db?.detail && <p>{db.detail}</p>}
      {db?.hint && (
        <p className="muted">
          {t.workspace.hint} {db.hint}
        </p>
      )}
    </div>
  );
}

/** Asks before saving to a production database, showing the statements that will run. */
function ConfirmSave({
  statements,
  target,
  onSave,
  onCancel,
}: {
  statements: string[];
  target: SaveTarget;
  onSave: () => void;
  onCancel: () => void;
}) {
  const t = useT();
  return (
    <div className="dialog-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <div
        className="dialog risk-dialog"
        role="alertdialog"
        aria-label={t.rows.confirmTitle}
        onKeyDown={(e) => e.key === "Escape" && onCancel()}
      >
        <header className="side-panel-header">
          <strong>{t.rows.confirmTitle}</strong>
          <span className="spacer" />
          <button onClick={onCancel} title={t.common.close}>
            ✕
          </button>
        </header>
        <div className="dialog-body">
          <p className="risk-target">
            {target.environment && <EnvironmentBadge environment={target.environment} />}
            <span>{t.risk.target(target.label, target.address)}</span>
          </p>
          <pre className="rows-sql">{statements.join("\n")}</pre>
        </div>
        <footer className="dialog-footer">
          <div className="actions">
            <span className="spacer" />
            {/* Cancelling is the default: Enter doesn't save. */}
            <button onClick={onCancel} autoFocus>
              {t.common.cancel}
            </button>
            <button className="primary danger-fill" onClick={onSave}>
              {t.rows.saveOnProduction}
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
}

/** Asks before results with unsaved changes to their rows are replaced. */
export function DiscardChanges({
  count,
  onDiscard,
  onCancel,
}: {
  count: number;
  onDiscard: () => void;
  onCancel: () => void;
}) {
  const t = useT();
  return (
    <div className="dialog-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <div
        className="dialog risk-dialog"
        role="alertdialog"
        aria-label={t.rows.discardTitle}
        onKeyDown={(e) => e.key === "Escape" && onCancel()}
      >
        <header className="side-panel-header">
          <strong>{t.rows.discardTitle}</strong>
          <span className="spacer" />
          <button onClick={onCancel} title={t.common.close}>
            ✕
          </button>
        </header>
        <div className="dialog-body">
          <p>{t.rows.discardBody(count)}</p>
        </div>
        <footer className="dialog-footer">
          <div className="actions">
            <span className="spacer" />
            <button onClick={onCancel} autoFocus>
              {t.common.cancel}
            </button>
            <button className="primary danger-fill" onClick={onDiscard}>
              {t.rows.discardContinue}
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
}

/** The statement on one line, to tell results apart; clicking selects it in the editor. */
export function StatementLine({ sql, onReveal }: { sql: string; onReveal: () => void }) {
  const t = useT();
  return (
    <button className="result-sql" onClick={onReveal} title={`${sql}\n\n${t.results.revealTitle}`}>
      {sql.replace(/\s+/g, " ").trim()}
    </button>
  );
}

/** What a result holds, e.g. "1,000 rows" or "3 rows affected". */
export function resultCount(result: StatementResult, t: Messages) {
  if (result.kind === "command") {
    return result.rowsAffected > 0 ? t.results.rowsAffected(result.rowsAffected) : t.results.completed;
  }
  return result.rows.length < result.totalRows
    ? t.results.firstRows(result.rows.length, result.totalRows)
    : t.results.rows(result.totalRows);
}
