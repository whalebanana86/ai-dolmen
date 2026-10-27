import { useRef, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { DELIMITERS, DataFormat, DataSource, Delimiter, Transferred, data, errorMessage, isAppError } from "./api";
import { useT } from "./settings";

interface Props {
  /** The session whose search path a query's names resolve in. */
  sessionId: number;
  source: DataSource;
  /** What is exported, for the title, e.g. "shop.orders". */
  label: string;
  /** The file name to suggest, without its extension. */
  fileName: string;
  /** The table INSERT statements go into, to start with. */
  insertTable: string;
  onClose: () => void;
}

type Phase =
  | { kind: "choosing" }
  | { kind: "running"; rows: number }
  | { kind: "done"; result: Transferred; path: string }
  | { kind: "failed"; error: unknown };

/** Saves a table's or a query's rows as CSV, JSON or INSERT statements. */
export function ExportDialog({ sessionId, source, label, fileName, insertTable, onClose }: Props) {
  const t = useT();
  const [kind, setKind] = useState<DataFormat["kind"]>("csv");
  const [delimiter, setDelimiter] = useState<Delimiter>("comma");
  const [header, setHeader] = useState(true);
  const [bom, setBom] = useState(false);
  const [table, setTable] = useState(insertTable);
  const [phase, setPhase] = useState<Phase>({ kind: "choosing" });
  const job = useRef(0);
  const running = phase.kind === "running";

  async function run() {
    const format: DataFormat =
      kind === "csv" ? { kind, delimiter, header, bom } : kind === "json" ? { kind } : { kind, table: table.trim() };
    const extension = kind === "json" ? "json" : kind === "insert" ? "sql" : delimiter === "tab" ? "tsv" : "csv";
    const path = await save({
      title: t.transfer.saveTitle,
      defaultPath: `${fileName}.${extension}`,
      filters: [{ name: t.transfer.formats[kind], extensions: [extension] }],
    });
    if (!path) return;
    job.current = Date.now();
    setPhase({ kind: "running", rows: 0 });
    try {
      const result = await data.export(sessionId, job.current, source, format, path, (rows) =>
        setPhase((current) => (current.kind === "running" ? { kind: "running", rows } : current)),
      );
      setPhase({ kind: "done", result, path });
    } catch (error) {
      setPhase({ kind: "failed", error });
    }
  }

  return (
    <div className="dialog-backdrop" onMouseDown={(e) => e.target === e.currentTarget && !running && onClose()}>
      <div
        className="dialog transfer-dialog"
        role="dialog"
        aria-label={t.transfer.exportTitle(label)}
        onKeyDown={(e) => e.key === "Escape" && !running && onClose()}
      >
        <header className="side-panel-header">
          <strong>{t.transfer.exportTitle(label)}</strong>
          <span className="spacer" />
          <button onClick={onClose} disabled={running} title={t.common.close}>
            ✕
          </button>
        </header>
        <div className="dialog-body">
          {source.kind === "query" && <p className="muted">{t.transfer.queryNote}</p>}
          <fieldset className="transfer-formats" disabled={running}>
            <legend>{t.transfer.format}</legend>
            {(["csv", "json", "insert"] as const).map((format) => (
              <label key={format} className="checkbox">
                <input type="radio" name="format" checked={kind === format} onChange={() => setKind(format)} />
                {t.transfer.formats[format]}
              </label>
            ))}
          </fieldset>
          {kind === "csv" && (
            <div className="transfer-options">
              <label className="field">
                {t.transfer.delimiter}
                <select
                  value={delimiter}
                  onChange={(e) => setDelimiter(e.target.value as Delimiter)}
                  disabled={running}
                >
                  {DELIMITERS.map((d) => (
                    <option key={d} value={d}>
                      {t.transfer.delimiters[d]}
                    </option>
                  ))}
                </select>
              </label>
              <label className="checkbox">
                <input type="checkbox" checked={header} onChange={(e) => setHeader(e.target.checked)} disabled={running} />
                {t.transfer.header}
              </label>
              <label className="checkbox">
                <input type="checkbox" checked={bom} onChange={(e) => setBom(e.target.checked)} disabled={running} />
                {t.transfer.bom}
              </label>
            </div>
          )}
          {kind === "insert" && (
            <label className="field">
              {t.transfer.insertTable}
              <input value={table} onChange={(e) => setTable(e.target.value)} disabled={running} spellCheck={false} />
            </label>
          )}
          {phase.kind === "running" && <p className="muted">{t.transfer.exporting(phase.rows)}</p>}
          {phase.kind === "done" && (
            <div className="transfer-done">
              <p>{t.transfer.exported(phase.result.rows, formatSize(phase.result.bytes))}</p>
              <p className="muted transfer-path" title={phase.path}>
                {phase.path}
              </p>
            </div>
          )}
          {phase.kind === "failed" && <TransferError error={phase.error} />}
        </div>
        <footer className="dialog-footer">
          <div className="actions">
            <span className="spacer" />
            {running ? (
              <button onClick={() => data.cancel(job.current).catch(() => {})}>{t.common.cancel}</button>
            ) : (
              <>
                <button onClick={onClose}>{t.common.close}</button>
                <button
                  className="primary"
                  onClick={run}
                  disabled={kind === "insert" && table.trim() === ""}
                >
                  {t.transfer.export}
                </button>
              </>
            )}
          </div>
        </footer>
      </div>
    </div>
  );
}

/** A failed export or import: the server's message with where it happened, or that it was stopped. */
export function TransferError({ error }: { error: unknown }) {
  const t = useT();
  if (isAppError(error) && error.kind === "database" && error.code === "57014") {
    return <p className="muted">{t.transfer.cancelled}</p>;
  }
  const db = isAppError(error) && error.kind === "database" ? error : null;
  return (
    <div className="error-box">
      <p>
        <strong>{errorMessage(error, t)}</strong>
      </p>
      {db && <p className="muted">SQLSTATE {db.code}</p>}
      {db?.detail && <pre className="transfer-detail">{db.detail}</pre>}
      {db?.hint && (
        <p className="muted">
          {t.workspace.hint} {db.hint}
        </p>
      )}
    </div>
  );
}

export function formatSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
