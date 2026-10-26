import { StatementResult } from "./api";
import { ResultGrid } from "./ResultGrid";

interface Props {
  index: number;
  result: StatementResult;
}

export function ResultView({ index, result }: Props) {
  if (result.kind === "command") {
    return (
      <div className="result">
        <p className="result-title">
          Statement {index + 1}:{" "}
          {result.rowsAffected > 0 ? `${result.rowsAffected.toLocaleString()} rows affected` : "completed"}
        </p>
      </div>
    );
  }

  const truncated = result.rows.length < result.totalRows;
  return (
    <div className="result">
      <p className="result-title">
        Statement {index + 1}:{" "}
        {truncated
          ? `showing first ${result.rows.length.toLocaleString()} of ${result.totalRows.toLocaleString()} rows`
          : `${result.totalRows.toLocaleString()} ${result.totalRows === 1 ? "row" : "rows"}`}
      </p>
      <ResultGrid columns={result.columns} rows={result.rows} />
    </div>
  );
}
