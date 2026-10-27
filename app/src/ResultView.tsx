import { StatementResult } from "./api";
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
}

export function ResultView({ index, result, sql, onReveal }: Props) {
  const t = useT();
  const table = statementTable(sql);
  const count =
    result.kind === "command"
      ? result.rowsAffected > 0
        ? t.results.rowsAffected(result.rowsAffected)
        : t.results.completed
      : result.rows.length < result.totalRows
        ? t.results.firstRows(result.rows.length, result.totalRows)
        : t.results.rows(result.totalRows);

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
