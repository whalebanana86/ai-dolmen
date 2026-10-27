import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { CsvPreview, DELIMITERS, Delimiter, FileEncoding, Table, Transferred, data } from "./api";
import { TransferError } from "./ExportDialog";
import { quoteIdent } from "./sqlIdent";
import { useT } from "./settings";

interface Props {
  sessionId: number;
  schema: string;
  table: Table;
  onClose: () => void;
}

type Phase =
  | { kind: "choosing" }
  | { kind: "running"; bytes: number }
  | { kind: "done"; result: Transferred }
  | { kind: "failed"; error: unknown };

/** Data rows shown under the file's columns. */
const SAMPLE_ROWS = 8;

/**
 * Appends a CSV file's rows to a table: pick the file, check how it reads,
 * say which table column each of its columns fills, and import in one
 * transaction.
 */
export function ImportDialog({ sessionId, schema, table, onClose }: Props) {
  const t = useT();
  const [path, setPath] = useState<string | null>(null);
  const [encoding, setEncoding] = useState<FileEncoding>("utf8");
  const [delimiter, setDelimiter] = useState<Delimiter | null>(null);
  const [header, setHeader] = useState(true);
  const [preview, setPreview] = useState<CsvPreview | null>(null);
  // Whether the user picked the encoding, and whether it was switched to EUC-KR for them.
  const [encodingChosen, setEncodingChosen] = useState(false);
  const [switchedToEucKr, setSwitchedToEucKr] = useState(false);
  const [previewError, setPreviewError] = useState<unknown>(null);
  const [mapping, setMapping] = useState<(string | null)[]>([]);
  const [phase, setPhase] = useState<Phase>({ kind: "choosing" });
  const job = useRef(0);
  const running = phase.kind === "running";
  const name = `${quoteIdent(schema)}.${quoteIdent(table.name)}`;
  // Generated columns can't be given values.
  const targets = table.columns.filter((column) => !column.generated);

  async function chooseFile() {
    const picked = await open({
      title: t.transfer.chooseFile,
      filters: [{ name: t.transfer.fileFilter, extensions: ["csv", "tsv", "txt"] }],
    });
    if (typeof picked !== "string") return;
    setPath(picked);
    setEncoding("utf8");
    setDelimiter(null);
    setEncodingChosen(false);
    setSwitchedToEucKr(false);
    setPhase({ kind: "choosing" });
  }

  useEffect(() => {
    chooseFile();
    // Only when the dialog opens.
  }, []);

  // Reads the start of the file again whenever how to read it changes.
  useEffect(() => {
    if (!path) return;
    let current = true;
    setPreviewError(null);
    data
      .preview(path, delimiter, encoding)
      .then((read) => {
        if (!current) return;
        // Korean Excel saves CSV in EUC-KR: read it that way unless the user chose otherwise.
        if (read.looksEucKr && encoding === "utf8" && !encodingChosen) {
          setSwitchedToEucKr(true);
          setEncoding("eucKr");
          return;
        }
        setPreview(read);
        if (delimiter === null) setDelimiter(read.delimiter);
      })
      .catch((error) => current && setPreviewError(error));
    return () => {
      current = false;
    };
  }, [path, delimiter, encoding]);

  const width = preview ? Math.max(0, ...preview.rows.map((row) => row.length)) : 0;
  const names = preview && header ? (preview.rows[0] ?? []) : [];
  const sample = preview ? preview.rows.slice(header ? 1 : 0, (header ? 1 : 0) + SAMPLE_ROWS) : [];

  // Fills each file column by name when there's a header, else in order.
  useEffect(() => {
    if (!preview) return;
    const byName = new Map(targets.map((column) => [column.name.toLowerCase(), column.name]));
    setMapping(
      Array.from({ length: width }, (_, i) =>
        header ? (byName.get((names[i] ?? "").trim().toLowerCase()) ?? null) : (targets[i]?.name ?? null),
      ),
    );
  }, [preview, header]);

  const chosen = mapping.filter((column): column is string => column !== null);
  const duplicate = chosen.find((column, i) => chosen.indexOf(column) !== i);
  const identity = targets.filter((column) => column.identity && chosen.includes(column.name)).map((c) => c.name);
  const empty = preview !== null && sample.length === 0;
  const canImport = !!path && !!preview && !running && chosen.length > 0 && !duplicate && !empty;

  async function run() {
    if (!path || !preview || !delimiter) return;
    job.current = Date.now();
    setPhase({ kind: "running", bytes: 0 });
    try {
      const result = await data.import(
        sessionId,
        job.current,
        schema,
        table.name,
        path,
        { delimiter, header, encoding },
        mapping,
        (bytes) => setPhase((current) => (current.kind === "running" ? { kind: "running", bytes } : current)),
      );
      setPhase({ kind: "done", result });
    } catch (error) {
      setPhase({ kind: "failed", error });
    }
  }

  const percent =
    phase.kind === "running" && preview?.size ? Math.min(100, Math.round((phase.bytes / preview.size) * 100)) : 0;

  return (
    <div className="dialog-backdrop" onMouseDown={(e) => e.target === e.currentTarget && !running && onClose()}>
      <div
        className="dialog transfer-dialog import-dialog"
        role="dialog"
        aria-label={t.transfer.importTitle(name)}
        onKeyDown={(e) => e.key === "Escape" && !running && onClose()}
      >
        <header className="side-panel-header">
          <strong>{t.transfer.importTitle(name)}</strong>
          <span className="spacer" />
          <button onClick={onClose} disabled={running} title={t.common.close}>
            ✕
          </button>
        </header>
        <div className="dialog-body">
          <div className="transfer-file">
            <button onClick={chooseFile} disabled={running}>
              {path ? t.transfer.otherFile : t.transfer.chooseFile}
            </button>
            {path && (
              <span className="muted transfer-path" title={path}>
                {path}
              </span>
            )}
          </div>

          {path && (
            <div className="transfer-options">
              <label className="field">
                {t.transfer.encoding}
                <select
                  value={encoding}
                  onChange={(e) => {
                    setEncodingChosen(true);
                    setEncoding(e.target.value as FileEncoding);
                  }}
                  disabled={running}
                >
                  {(["utf8", "eucKr"] as const).map((e) => (
                    <option key={e} value={e}>
                      {t.transfer.encodings[e]}
                    </option>
                  ))}
                </select>
              </label>
              <label className="field">
                {t.transfer.delimiter}
                <select
                  value={delimiter ?? ""}
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
            </div>
          )}
          {switchedToEucKr && !encodingChosen && <p className="muted">{t.transfer.readAsEucKr}</p>}
          {previewError !== null && <TransferError error={previewError} />}
          {empty && <p className="muted">{t.transfer.emptyFile}</p>}

          {preview && width > 0 && !empty && (
            <>
              <p className="muted">{t.transfer.firstRows(sample.length)}</p>
              <div className="import-preview">
                <table>
                  <thead>
                    <tr>
                      {mapping.map((target, i) => (
                        <th key={i}>
                          <select
                            value={target ?? ""}
                            onChange={(e) =>
                              setMapping((current) => current.map((c, j) => (j === i ? e.target.value || null : c)))
                            }
                            disabled={running}
                            className={target ? undefined : "skipped"}
                            aria-label={names[i] || t.transfer.column(i + 1)}
                          >
                            <option value="">{t.transfer.skip}</option>
                            {targets.map((column) => (
                              <option key={column.name} value={column.name}>
                                {column.name} · {column.dataType}
                              </option>
                            ))}
                          </select>
                        </th>
                      ))}
                    </tr>
                    <tr>
                      {mapping.map((_, i) => (
                        <th key={i} className="import-source">
                          {names[i] || t.transfer.column(i + 1)}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {sample.map((row, r) => (
                      <tr key={r}>
                        {mapping.map((target, i) => (
                          <td key={i} className={target ? undefined : "skipped"}>
                            {row[i] ?? ""}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              {duplicate && <p className="error-text">{t.transfer.duplicate(duplicate)}</p>}
              {chosen.length === 0 && <p className="error-text">{t.transfer.noneMapped}</p>}
              {identity.length > 0 && <p className="muted">{t.transfer.identity(identity.join(", "))}</p>}
              <p className="muted">{t.transfer.allOrNothing}</p>
            </>
          )}

          {phase.kind === "running" && (
            <div className="transfer-progress">
              <span>{t.transfer.importing(percent)}</span>
              <span className="plan-bar">
                <span style={{ width: `${percent}%` }} />
              </span>
            </div>
          )}
          {phase.kind === "done" && <p>{t.transfer.imported(phase.result.rows, name)}</p>}
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
                {phase.kind !== "done" && (
                  <button className="primary" onClick={run} disabled={!canImport}>
                    {t.transfer.import}
                  </button>
                )}
              </>
            )}
          </div>
        </footer>
      </div>
    </div>
  );
}
