import { StatementResult } from "./api";

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
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th className="row-number">#</th>
              {result.columns.map((column, i) => (
                <th key={i}>{column}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {result.rows.map((row, r) => (
              <tr key={r}>
                <td className="row-number">{r + 1}</td>
                {row.map((value, c) =>
                  value === null ? (
                    <td key={c} className="null">
                      NULL
                    </td>
                  ) : (
                    <td key={c} title={value}>
                      {value}
                    </td>
                  ),
                )}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
