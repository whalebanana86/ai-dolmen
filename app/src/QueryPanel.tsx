import { useCallback, useEffect, useRef, useState } from "react";
import { EditorView } from "@codemirror/view";
import {
  Introspection,
  SavedConnection,
  SchemaObject,
  SessionInfo,
  SnapshotStatus,
  StatementResult,
  TextRange,
  connections,
  errorMessage,
  isAppError,
  pg,
  snapshots,
  sqlText,
} from "./api";
import { ResultView } from "./ResultView";
import { SchemaTree } from "./SchemaTree";
import { SnapshotPanel } from "./SnapshotPanel";
import { SqlEditor } from "./SqlEditor";

const INITIAL_SQL = "SELECT version();\n";

interface Location {
  line: number;
  column: number;
}

interface Outcome {
  /** Distinguishes runs, so result grids start fresh instead of keeping old column widths. */
  runId: number;
  /** Results of the statements that succeeded, in order. */
  results: StatementResult[];
  /** The error that stopped the run, if any. */
  failure: { error: unknown; location: Location | null } | null;
  statementCount: number;
  firstLine: number;
  elapsedMs: number;
}

interface Props {
  session: SessionInfo;
  /** The saved connection the session came from, if any. */
  connection: SavedConnection | null;
  onConnectionChange: (connection: SavedConnection) => void;
  onDisconnect: () => void;
}

export function QueryPanel({ session, connection, onConnectionChange, onDisconnect }: Props) {
  const viewRef = useRef<EditorView | null>(null);
  const [hasText, setHasText] = useState(INITIAL_SQL.trim() !== "");
  const [introspection, setIntrospection] = useState<Introspection | null>(null);
  const [loadingSchema, setLoadingSchema] = useState(false);
  const [running, setRunning] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const runCount = useRef(0);

  const refreshSchema = useCallback(() => {
    setLoadingSchema(true);
    pg.introspect(session.id)
      .then(setIntrospection)
      .catch((error) => console.error("loading the database structure failed", error))
      .finally(() => setLoadingSchema(false));
  }, [session.id]);
  useEffect(refreshSchema, [refreshSchema]);

  // An unsaved connection can still snapshot; its folder is just not remembered.
  const [snapshotDir, setSnapshotDir] = useState<string | null>(connection?.snapshotDir ?? null);
  const [snapshotStatus, setSnapshotStatus] = useState<SnapshotStatus | null>(null);
  const [snapshotError, setSnapshotError] = useState<string | null>(null);
  const [checkingSnapshot, setCheckingSnapshot] = useState(false);
  const [snapshotOpen, setSnapshotOpen] = useState(false);

  const refreshSnapshot = useCallback(() => {
    if (!snapshotDir) return;
    setCheckingSnapshot(true);
    snapshots
      .status(session.id, snapshotDir)
      .then((status) => {
        setSnapshotStatus(status);
        setSnapshotError(null);
      })
      .catch((error) => setSnapshotError(errorMessage(error)))
      .finally(() => setCheckingSnapshot(false));
  }, [session.id, snapshotDir]);
  useEffect(refreshSnapshot, [refreshSnapshot]);

  async function chooseSnapshotDir(dir: string) {
    setSnapshotStatus(null);
    setSnapshotDir(dir);
    if (connection) {
      try {
        onConnectionChange(await connections.save({ ...connection, snapshotDir: dir }, { kind: "keep" }));
      } catch (error) {
        setSnapshotError(`The folder is in use but couldn't be saved with the connection: ${errorMessage(error)}`);
      }
    }
  }

  async function run() {
    const view = viewRef.current;
    if (running || !view) return;
    const sql = view.state.doc.toString();
    const { from, to } = view.state.selection.main;
    const ranges = await statementsToRun(sql, from, to);
    if (ranges.length === 0) return;

    setRunning(true);
    const started = performance.now();
    const results: StatementResult[] = [];
    let failure: Outcome["failure"] = null;
    // One statement at a time, so a failure doesn't roll back the ones before it.
    for (const range of ranges) {
      const statement = sql.slice(range.start, range.end);
      try {
        results.push(...(await pg.execute(session.id, statement)));
      } catch (error) {
        failure = { error, location: errorLocation(sql, range.start, statement, error) };
        break;
      }
    }
    setOutcome({
      runId: ++runCount.current,
      results,
      failure,
      statementCount: ranges.length,
      firstLine: lineAt(sql, ranges[0].start),
      elapsedMs: performance.now() - started,
    });
    setRunning(false);
    // DDL such as CREATE TABLE returns no rows; reload the tree, completion and snapshot status.
    if (results.some((result) => result.kind === "command")) {
      refreshSchema();
      refreshSnapshot();
    }
  }

  function showDdl(schema: string, object: SchemaObject) {
    snapshots
      .createStatement(schema, object)
      .then((ddl) => insertStatement(ddl.trimEnd(), false))
      .catch((error) => console.error("generating DDL failed", error));
  }

  /** Adds a statement at the end of the editor, puts the cursor on it, and runs it if asked. */
  function insertStatement(statement: string, runIt: boolean) {
    const view = viewRef.current;
    if (!view) return;
    const doc = view.state.doc;
    const separator = doc.length === 0 || doc.toString().endsWith("\n") ? "" : "\n";
    const start = doc.length + separator.length;
    view.dispatch({
      changes: { from: doc.length, insert: `${separator}${statement}\n` },
      selection: { anchor: start },
      scrollIntoView: true,
    });
    view.focus();
    if (runIt) run();
  }

  function cancel() {
    // The running statement fails with query_canceled, which stops the run.
    pg.cancel(session.id).catch((error) => console.error("cancel failed", error));
  }

  async function disconnect() {
    await pg.disconnect(session.id);
    onDisconnect();
  }

  return (
    <main className="workspace">
      <div className="toolbar">
        <strong>Dolmen</strong>
        <span className="muted">PostgreSQL {session.serverVersion}</span>
        <span className="spacer" />
        {running ? (
          <button onClick={cancel}>Cancel</button>
        ) : (
          <button
            className="primary"
            onClick={run}
            disabled={!hasText}
            title="Runs the statement at the cursor, or the selection (⌘/Ctrl + Enter)"
          >
            Run
          </button>
        )}
        <button onClick={() => setSnapshotOpen(true)} title="Compare the database with its schema snapshot">
          Snapshot
          {snapshotStatus && (
            <span className={`badge ${snapshotStatus.changes.length > 0 ? "" : "ok"}`}>
              {snapshotStatus.changes.length > 0 ? snapshotStatus.changes.length : "✓"}
            </span>
          )}
        </button>
        <button onClick={disconnect}>Disconnect</button>
      </div>

      <div className="workspace-body">
        <SchemaTree
          introspection={introspection}
          loading={loadingSchema}
          actions={{ insert: insertStatement, refresh: refreshSchema, showDdl }}
        />
        <div className="workspace-main">
          <SqlEditor
            initialValue={INITIAL_SQL}
            introspection={introspection}
            onRun={run}
            onChange={(value) => setHasText(value.trim() !== "")}
            viewRef={viewRef}
          />

          <section className="results">
            {running && <p className="muted status">Running…</p>}
            {!running && outcome && (
              <>
                <p className="muted status">
                  Line {outcome.firstLine} · {outcome.results.length} of {outcome.statementCount}{" "}
                  {outcome.statementCount === 1 ? "statement" : "statements"} ·{" "}
                  {Math.round(outcome.elapsedMs)} ms
                </p>
                {outcome.results.map((result, index) => (
                  <ResultView key={`${outcome.runId}-${index}`} index={index} result={result} />
                ))}
                {outcome.failure && <ErrorView {...outcome.failure} />}
              </>
            )}
          </section>
        </div>
      </div>
      {snapshotOpen && (
        <SnapshotPanel
          sessionId={session.id}
          database={session.database}
          dir={snapshotDir}
          onChooseDir={chooseSnapshotDir}
          status={snapshotStatus}
          statusError={snapshotError}
          checking={checkingSnapshot}
          onRefresh={refreshSnapshot}
          onDatabaseChanged={refreshSchema}
          onClose={() => setSnapshotOpen(false)}
        />
      )}
    </main>
  );
}

/** The selected statements, or the statement at the cursor when nothing is selected. */
async function statementsToRun(sql: string, selectionStart: number, selectionEnd: number) {
  if (selectionStart === selectionEnd) {
    const range = await sqlText.statementAt(sql, selectionStart);
    return range ? [range] : [];
  }
  const ranges = await sqlText.split(sql.slice(selectionStart, selectionEnd));
  return ranges.map(
    (range): TextRange => ({ start: range.start + selectionStart, end: range.end + selectionStart }),
  );
}

function ErrorView({ error, location }: { error: unknown; location: Location | null }) {
  const db = isAppError(error) && error.kind === "database" ? error : null;

  return (
    <div className="error-box">
      <p>
        <strong>{errorMessage(error)}</strong>
      </p>
      {db && (
        <p className="muted">
          SQLSTATE {db.code}
          {location && ` · line ${location.line}, column ${location.column}`}
        </p>
      )}
      {db?.detail && <p>{db.detail}</p>}
      {db?.hint && <p className="muted">Hint: {db.hint}</p>}
    </div>
  );
}

function lineAt(text: string, index: number) {
  return text.slice(0, index).split("\n").length;
}

/**
 * Maps the error position PostgreSQL reports within `statement` to a line and
 * column in the editor. PostgreSQL counts characters, so split by code point.
 */
function errorLocation(
  sql: string,
  statementStart: number,
  statement: string,
  error: unknown,
): Location | null {
  if (!isAppError(error) || error.kind !== "database" || !error.position) return null;
  const before = sql.slice(0, statementStart) + [...statement].slice(0, error.position - 1).join("");
  const lines = before.split("\n");
  return { line: lines.length, column: [...lines[lines.length - 1]].length + 1 };
}
