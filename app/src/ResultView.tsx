import { StatementResult } from "./api";
import { ResultGrid } from "./ResultGrid";
import { useT } from "./settings";

interface Props {
  index: number;
  result: StatementResult;
}

export function ResultView({ index, result }: Props) {
  const t = useT();
  if (result.kind === "command") {
    return (
      <div className="result">
        <p className="result-title">
          {t.results.statement(index + 1)}:{" "}
          {result.rowsAffected > 0 ? t.results.rowsAffected(result.rowsAffected) : t.results.completed}
        </p>
      </div>
    );
  }

  const truncated = result.rows.length < result.totalRows;
  return (
    <div className="result">
      <p className="result-title">
        {t.results.statement(index + 1)}:{" "}
        {truncated ? t.results.firstRows(result.rows.length, result.totalRows) : t.results.rows(result.totalRows)}
      </p>
      <ResultGrid columns={result.columns} rows={result.rows} />
    </div>
  );
}
