import { StatementResult } from "./api";
import type { Messages } from "./i18n/en";
import { ResultGrid } from "./ResultGrid";
import { useT } from "./settings";
import { statementTable } from "./sqlIdent";

interface Props {
  index: number;
  result: StatementResult;
  /** The statement that gave the result. */
  sql: string;
  /** Selects the statement in the editor. */
  onReveal: () => void;
  /** Saves the statement's rows to a file. */
  onExport: () => void;
}

export function ResultView({ index, result, sql, onReveal, onExport }: Props) {
  const t = useT();
  const table = statementTable(sql);
  const count = resultCount(result, t);

  return (
    <div className="result">
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
        {result.kind === "rows" && result.columns.length > 0 && (
          <button className="result-export" onClick={onExport} title={t.results.exportTitle}>
            {t.results.export}
          </button>
        )}
      </p>
      <StatementLine sql={sql} onReveal={onReveal} />
      {result.kind === "rows" && <ResultGrid columns={result.columns} rows={result.rows} />}
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
