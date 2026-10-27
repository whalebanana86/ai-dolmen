import { Suspense, lazy, useCallback, useEffect, useRef, useState } from "react";
import { EditorView } from "@codemirror/view";
import {
  Introspection,
  ObjectRef,
  SavedConnection,
  SnapshotRemote,
  SchemaObject,
  SessionInfo,
  SnapshotStatus,
  StatementResult,
  TextRange,
  connections,
  errorMessage,
  history,
  isAppError,
  AiTask,
  pg,
  snapshots,
  sqlText,
} from "./api";
import { AiPanel, FixTarget, useAi } from "./AiPanel";
import { CreatePanel, DetailPanel } from "./DetailPanel";
import { ErrorBoundary } from "./ErrorBoundary";
import { HistoryPanel } from "./HistoryPanel";
import { defaultLayout, useLayout } from "./layout";
import { Splitter } from "./Splitter";
import { ResultView } from "./ResultView";
import { SchemaTree } from "./SchemaTree";
import { SettingsButton, SettingsDialog } from "./SettingsDialog";
import { SnapshotPanel } from "./SnapshotPanel";
import { SqlEditor } from "./SqlEditor";
import type { Messages } from "./i18n/en";
import { useT } from "./settings";

const INITIAL_SQL = "SELECT version();\n";

// The ERD brings a diagram library, so it loads when first opened.
const ErdView = lazy(() => import("./ErdView").then((module) => ({ default: module.ErdView })));

interface Location {
  line: number;
  column: number;
}

interface Outcome {
  /** Distinguishes runs, so result grids start fresh instead of keeping old column widths. */
  runId: number;
  /** Results of the statements that succeeded, in order. */
  results: StatementResult[];
  /** The error that stopped the run, if any, and the statement that failed. */
  failure: { error: unknown; location: Location | null; statement: FixTarget } | null;
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
  const t = useT();
  // For callbacks that must not rerun when only the language changes.
  const tRef = useRef(t);
  useEffect(() => {
    tRef.current = t;
  });
  const [settingsOpen, setSettingsOpen] = useState(false);
  const viewRef = useRef<EditorView | null>(null);
  const [hasText, setHasText] = useState(INITIAL_SQL.trim() !== "");
  // The editor keeps each connection's text, saved once typing pauses.
  const editorKey = session.savedId ?? session.label;
  const [initialSql, setInitialSql] = useState<string | null>(null);
  const editorText = useRef<string | null>(null);
  const saveTimer = useRef<number | undefined>(undefined);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [introspection, setIntrospection] = useState<Introspection | null>(null);
  const [loadingSchema, setLoadingSchema] = useState(false);
  const [running, setRunning] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const runCount = useRef(0);
  const { conversation, ask, stop: stopAi, clear: clearAi } = useAi(session.id);
  const [aiPrompting, setAiPrompting] = useState(false);
  const { layout, update: updateLayout, toggle: togglePanel } = useLayout();
  const [detail, setDetail] = useState<ObjectRef | null>(null);
  const [creating, setCreating] = useState<{ type: "table" | "view" | "function"; schema: string } | null>(null);
  const [erd, setErd] = useState<{ schema: string | null; focus: string | null } | null>(null);
  const mainRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    history
      .loadEditor(editorKey)
      .catch(() => null)
      .then((saved) => {
        const text = saved ?? INITIAL_SQL;
        setInitialSql(text);
        setHasText(text.trim() !== "");
      });
  }, [editorKey]);

  /** Saves the editor's text now, instead of after the pause. */
  const saveEditor = useCallback(() => {
    clearTimeout(saveTimer.current);
    saveTimer.current = undefined;
    if (editorText.current !== null) {
      history.saveEditor(editorKey, editorText.current).catch((error) => console.error("saving the editor failed", error));
    }
  }, [editorKey]);
  useEffect(() => saveEditor, [saveEditor]);

  function editorChanged(value: string) {
    setHasText(value.trim() !== "");
    editorText.current = value;
    clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(saveEditor, 800);
  }

  // ⌘B and ⌘J show and hide the schema tree and the results, as in VS Code; ⌘⇧H opens the history.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && !e.altKey && e.key.toLowerCase() === "h") {
        e.preventDefault();
        setHistoryOpen((open) => !open);
        return;
      }
      if (!(e.metaKey || e.ctrlKey) || e.shiftKey || e.altKey) return;
      const panel = { b: "treeVisible", j: "resultsVisible" } as const;
      const key = e.key.toLowerCase();
      if (key !== "b" && key !== "j") return;
      e.preventDefault();
      togglePanel(panel[key]);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const refreshSchema = useCallback(() => {
    setLoadingSchema(true);
    return pg
      .introspect(session.id)
      .then(setIntrospection)
      .catch((error) => console.error("loading the database structure failed", error))
      .finally(() => setLoadingSchema(false));
  }, [session.id]);
  useEffect(() => {
    refreshSchema();
  }, [refreshSchema]);

  // An unsaved connection can still snapshot; its location is just not remembered.
  // With a remote, snapshotDir is the folder inside Dolmen's clone once it's ready.
  const [snapshotRemote, setSnapshotRemote] = useState<SnapshotRemote | null>(
    connection?.snapshotRemote ?? null,
  );
  const [snapshotDir, setSnapshotDir] = useState<string | null>(
    connection?.snapshotRemote ? null : (connection?.snapshotDir ?? null),
  );
  const [clonePath, setClonePath] = useState<string | null>(null);
  /** Why the last pull failed, while the clone is used as it is. */
  const [syncWarning, setSyncWarning] = useState<string | null>(null);
  const [preparingRemote, setPreparingRemote] = useState(false);
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
      .catch((error) => setSnapshotError(errorMessage(error, tRef.current)))
      .finally(() => setCheckingSnapshot(false));
  }, [session.id, snapshotDir]);
  useEffect(refreshSnapshot, [refreshSnapshot]);

  /** Clones the remote, or pulls new commits into Dolmen's clone of it. */
  const syncRemote = useCallback(async (remote: SnapshotRemote) => {
    setPreparingRemote(true);
    try {
      const checkout = await snapshots.checkout(remote.url, remote.path);
      setClonePath(checkout.clone);
      setSnapshotDir(checkout.dir);
      setSnapshotError(null);
      setSyncWarning(checkout.syncError && tRef.current.snapshot.syncFailed(checkout.syncError));
      return checkout;
    } catch (error) {
      setSnapshotError(errorMessage(error, tRef.current));
      throw error;
    } finally {
      setPreparingRemote(false);
    }
  }, []);
  useEffect(() => {
    if (snapshotRemote) syncRemote(snapshotRemote).catch(() => {});
  }, [snapshotRemote, syncRemote]);

  /** Remembers the snapshot location with the saved connection, if there is one. */
  async function saveLocation(location: Pick<SavedConnection, "snapshotDir" | "snapshotRemote">) {
    if (!connection) return;
    try {
      onConnectionChange(await connections.save({ ...connection, ...location }, { kind: "keep" }));
    } catch (error) {
      setSnapshotError(t.workspace.locationNotSaved(errorMessage(error, t)));
    }
  }

  async function chooseSnapshotDir(dir: string) {
    setSnapshotStatus(null);
    setSnapshotRemote(null);
    setClonePath(null);
    setSnapshotDir(dir);
    await saveLocation({ snapshotDir: dir, snapshotRemote: null });
  }

  async function chooseSnapshotRemote(remote: SnapshotRemote) {
    setSnapshotStatus(null);
    setSnapshotDir(null);
    setSnapshotRemote(remote);
    await saveLocation({ snapshotDir: null, snapshotRemote: remote });
  }

  /** Back to choosing a location; nothing is saved until a new one is chosen. */
  function resetSnapshotLocation() {
    setSnapshotStatus(null);
    setSnapshotError(null);
    setSnapshotRemote(null);
    setClonePath(null);
    setSnapshotDir(null);
  }

  async function run() {
    const view = viewRef.current;
    if (running || !view) return;
    const sql = view.state.doc.toString();
    const { from, to } = view.state.selection.main;
    const ranges = await statementsToRun(sql, from, to);
    if (ranges.length === 0) return;

    setRunning(true);
    updateLayout({ resultsVisible: true });
    const started = performance.now();
    const results: StatementResult[] = [];
    let failure: Outcome["failure"] = null;
    // One statement at a time, so a failure doesn't roll back the ones before it.
    for (const range of ranges) {
      const statement = sql.slice(range.start, range.end);
      try {
        results.push(...(await pg.execute(session.id, statement)));
      } catch (error) {
        failure = {
          error,
          location: errorLocation(sql, range.start, statement, error),
          statement: { statement, start: range.start, end: range.end },
        };
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

  /** The selected text, or the statement at the cursor. */
  async function editorSql() {
    const view = viewRef.current;
    if (!view) return null;
    const sql = view.state.doc.toString();
    const { from, to } = view.state.selection.main;
    if (from !== to) return sql.slice(from, to);
    const range = await sqlText.statementAt(sql, from);
    return range ? sql.slice(range.start, range.end) : null;
  }

  async function askAi(task: (sql: string | null) => AiTask | null) {
    const asked = task(await editorSql());
    if (asked) ask(asked);
  }

  function fixWithAi(failure: NonNullable<Outcome["failure"]>) {
    setAiPrompting(false);
    ask({ kind: "fix", sql: failure.statement.statement, error: describeError(failure.error, t) }, failure.statement);
  }

  /** Puts a fix where the failed statement was, if the editor still has it there. */
  function replaceStatement(sql: string, target: FixTarget) {
    const view = viewRef.current;
    if (!view) return;
    if (view.state.sliceDoc(target.start, target.end) !== target.statement) {
      insertStatement(sql, false);
      return;
    }
    view.dispatch({
      changes: { from: target.start, to: target.end, insert: sql },
      selection: { anchor: target.start },
      scrollIntoView: true,
    });
    view.focus();
  }

  function closeAi() {
    clearAi();
    setAiPrompting(false);
    viewRef.current?.focus();
  }

  function cancel() {
    // The running statement fails with query_canceled, which stops the run.
    pg.cancel(session.id).catch((error) => console.error("cancel failed", error));
  }

  async function disconnect() {
    saveEditor();
    await pg.disconnect(session.id);
    onDisconnect();
  }

  return (
    <main className="workspace">
      <div className="toolbar">
        <strong>Dolmen</strong>
        <span className="muted">PostgreSQL {session.serverVersion}</span>
        <button
          className="icon-button panel-toggle"
          onClick={() => togglePanel("treeVisible")}
          aria-pressed={layout.treeVisible}
          title={t.workspace.toggleTree(layout.treeVisible)}
          aria-label={t.workspace.toggleTree(layout.treeVisible)}
        >
          <PanelIcon side="left" shown={layout.treeVisible} />
        </button>
        <button
          className="icon-button panel-toggle"
          onClick={() => togglePanel("resultsVisible")}
          aria-pressed={layout.resultsVisible}
          title={t.workspace.toggleResults(layout.resultsVisible)}
          aria-label={t.workspace.toggleResults(layout.resultsVisible)}
        >
          <PanelIcon side="bottom" shown={layout.resultsVisible} />
        </button>
        <span className="spacer" />
        {running ? (
          <button onClick={cancel}>{t.common.cancel}</button>
        ) : (
          <button
            className="primary"
            onClick={run}
            disabled={!hasText}
            title={t.workspace.runTitle}
          >
            {t.workspace.run}
          </button>
        )}
        <button onClick={() => setAiPrompting(true)} title={t.ai.buttonTitle}>
          {t.ai.button}
        </button>
        <button onClick={() => setHistoryOpen(true)} title={t.history.buttonTitle}>
          {t.history.button}
        </button>
        <button
          onClick={() => setErd({ schema: introspection?.defaultSchema ?? null, focus: null })}
          disabled={!introspection}
          title={t.workspace.erdTitle}
        >
          {t.workspace.erd}
        </button>
        <button onClick={() => setSnapshotOpen(true)} title={t.workspace.snapshotTitle}>
          {t.workspace.snapshot}
          {snapshotStatus && (
            <span className={`badge ${snapshotStatus.changes.length > 0 ? "" : "ok"}`}>
              {snapshotStatus.changes.length > 0 ? snapshotStatus.changes.length : "✓"}
            </span>
          )}
        </button>
        <button onClick={disconnect}>{t.workspace.disconnect}</button>
        <SettingsButton onClick={() => setSettingsOpen(true)} />
      </div>

      <div className="workspace-body">
        {layout.treeVisible && (
          <>
            <SchemaTree
              introspection={introspection}
              loading={loadingSchema}
              actions={{
                insert: insertStatement,
                refresh: refreshSchema,
                showDdl,
                showDetail: setDetail,
                showErd: (schema, focus) => setErd({ schema, focus }),
                create: (schema, type) => setCreating({ schema, type }),
              }}
              width={layout.treeWidth}
            />
            <Splitter
              orientation="vertical"
              size={layout.treeWidth}
              bounds={() => [160, window.innerWidth * 0.6]}
              grows={1}
              onResize={(treeWidth) => updateLayout({ treeWidth })}
              onReset={() => updateLayout({ treeWidth: defaultLayout().treeWidth })}
              label={t.workspace.resizeTree}
            />
          </>
        )}
        <div className="workspace-main" ref={mainRef}>
          {initialSql === null ? (
            // The saved text loads in a moment; the editor is made with it.
            <div
              className={layout.resultsVisible ? "editor" : "editor fill"}
              style={layout.resultsVisible ? { height: layout.editorHeight } : undefined}
            />
          ) : (
            <SqlEditor
              initialValue={initialSql}
              introspection={introspection}
              onRun={run}
              onAsk={() => setAiPrompting(true)}
              onChange={editorChanged}
              viewRef={viewRef}
              height={layout.resultsVisible ? layout.editorHeight : null}
            />
          )}
          {layout.resultsVisible && (
            <Splitter
              orientation="horizontal"
              size={layout.editorHeight}
              bounds={() => [80, (mainRef.current?.clientHeight ?? window.innerHeight) - 120]}
              grows={1}
              onResize={(editorHeight) => updateLayout({ editorHeight })}
              onReset={() => updateLayout({ editorHeight: defaultLayout().editorHeight })}
              label={t.workspace.resizeEditor}
            />
          )}

          {(aiPrompting || conversation) && (
            <AiPanel
              conversation={conversation}
              prompting={aiPrompting}
              onGenerate={(request) => askAi((context) => ({ kind: "generate", request, context }))}
              onExplain={() => askAi((sql) => (sql?.trim() ? { kind: "explain", sql } : null))}
              onStop={stopAi}
              onClose={closeAi}
              onInsert={(sql) => insertStatement(sql, false)}
              onReplace={replaceStatement}
              onOpenSettings={() => setSettingsOpen(true)}
            />
          )}

          {layout.resultsVisible && (
            <section className="results">
              {running && <p className="muted status">{t.workspace.running}</p>}
              {!running && outcome && (
                <>
                  <p className="muted status">
                    {t.workspace.runStatus(
                      outcome.firstLine,
                      outcome.results.length,
                      outcome.statementCount,
                      Math.round(outcome.elapsedMs),
                    )}
                  </p>
                  {outcome.results.map((result, index) => (
                    <ResultView key={`${outcome.runId}-${index}`} index={index} result={result} />
                  ))}
                  {outcome.failure && (
                    <ErrorView {...outcome.failure} onFix={() => fixWithAi(outcome.failure!)} />
                  )}
                </>
              )}
            </section>
          )}
        </div>
      </div>
      {erd && introspection && (
        <ErrorBoundary
          fallback={(error) => (
            <div className="dialog-backdrop" onMouseDown={() => setErd(null)}>
              <div className="dialog" role="alertdialog">
                <div className="dialog-body">
                  <p className="error-text">{t.erd.failed(error.message)}</p>
                </div>
                <footer className="dialog-footer">
                  <div className="actions">
                    <span className="spacer" />
                    <button onClick={() => setErd(null)}>{t.common.close}</button>
                  </div>
                </footer>
              </div>
            </div>
          )}
        >
          <Suspense fallback={null}>
            <ErdView
              introspection={introspection}
              schema={erd.schema}
              focus={erd.focus}
              onOpenDetail={setDetail}
              onClose={() => setErd(null)}
            />
          </Suspense>
        </ErrorBoundary>
      )}
      {detail && introspection && (
        <DetailPanel
          sessionId={session.id}
          introspection={introspection}
          target={detail}
          onNavigate={setDetail}
          onShowErd={(schema, table) => {
            setDetail(null);
            setErd({ schema, focus: table });
          }}
          onInsert={(sql) => {
            setDetail(null);
            insertStatement(sql, false);
          }}
          onChanged={(renamed) => {
            refreshSchema();
            refreshSnapshot();
            if (renamed) setDetail(renamed);
          }}
          onClose={() => setDetail(null)}
          width={layout.detailWidth}
          onResize={(detailWidth) => updateLayout({ detailWidth })}
          onResetWidth={() => updateLayout({ detailWidth: defaultLayout().detailWidth })}
        />
      )}
      {creating && introspection && (
        <CreatePanel
          sessionId={session.id}
          introspection={introspection}
          type={creating.type}
          schema={creating.schema}
          onCreated={async (created) => {
            setCreating(null);
            refreshSnapshot();
            // Show the new object's details once the tree knows it.
            await refreshSchema();
            if (created) setDetail(created);
          }}
          onInsert={(sql) => {
            setCreating(null);
            insertStatement(sql, false);
          }}
          onClose={() => setCreating(null)}
          width={layout.detailWidth}
          onResize={(detailWidth) => updateLayout({ detailWidth })}
          onResetWidth={() => updateLayout({ detailWidth: defaultLayout().detailWidth })}
        />
      )}
      {historyOpen && (
        <HistoryPanel
          session={session}
          recording={connection?.recordHistory ?? true}
          onInsert={(sql) => {
            setHistoryOpen(false);
            insertStatement(sql, false);
          }}
          onRun={(sql) => {
            setHistoryOpen(false);
            insertStatement(sql, true);
          }}
          onClose={() => setHistoryOpen(false)}
          width={layout.detailWidth}
          onResize={(detailWidth) => updateLayout({ detailWidth })}
          onResetWidth={() => updateLayout({ detailWidth: defaultLayout().detailWidth })}
        />
      )}
      {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}
      {snapshotOpen && (
        <SnapshotPanel
          sessionId={session.id}
          target={session}
          database={session.database}
          dir={snapshotDir}
          remote={snapshotRemote}
          clonePath={clonePath}
          syncWarning={syncWarning}
          preparingRemote={preparingRemote}
          onChooseDir={chooseSnapshotDir}
          onChooseRemote={chooseSnapshotRemote}
          onSyncRemote={async () => (snapshotRemote ? (await syncRemote(snapshotRemote)).updated : false)}
          onResetLocation={resetSnapshotLocation}
          status={snapshotStatus}
          statusError={snapshotError}
          checking={checkingSnapshot}
          onRefresh={refreshSnapshot}
          onDatabaseChanged={refreshSchema}
          onClose={() => setSnapshotOpen(false)}
          width={layout.snapshotWidth}
          onResize={(snapshotWidth) => updateLayout({ snapshotWidth })}
          onResetWidth={() => updateLayout({ snapshotWidth: defaultLayout().snapshotWidth })}
        />
      )}
    </main>
  );
}

/** A panel-layout icon with the left or bottom panel filled in while it shows. */
function PanelIcon({ side, shown }: { side: "left" | "bottom"; shown: boolean }) {
  const panel =
    side === "left" ? { x: 2, y: 3, width: 4.5, height: 10 } : { x: 2, y: 9, width: 12, height: 4 };
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
      <rect x="1.5" y="2.5" width="13" height="11" rx="2" fill="none" stroke="currentColor" />
      {side === "left" ? (
        <line x1="6.5" y1="2.5" x2="6.5" y2="13.5" stroke="currentColor" />
      ) : (
        <line x1="1.5" y1="9.5" x2="14.5" y2="9.5" stroke="currentColor" />
      )}
      {shown && <rect {...panel} rx="1" fill="currentColor" opacity="0.45" />}
    </svg>
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

function ErrorView({
  error,
  location,
  onFix,
}: {
  error: unknown;
  location: Location | null;
  onFix: () => void;
}) {
  const t = useT();
  const db = isAppError(error) && error.kind === "database" ? error : null;

  return (
    <div className="error-box">
      <p>
        <strong>{errorMessage(error, t)}</strong>
      </p>
      {db && (
        <p className="muted">
          SQLSTATE {db.code}
          {location && ` · ${t.workspace.errorLocation(location.line, location.column)}`}
        </p>
      )}
      {db?.detail && <p>{db.detail}</p>}
      {db?.hint && (
        <p className="muted">
          {t.workspace.hint} {db.hint}
        </p>
      )}
      {/* Only the server's errors are about the SQL; connection problems aren't. */}
      {db && db.code !== "57014" && (
        <div className="actions">
          <button onClick={onFix}>{t.ai.fix}</button>
        </div>
      )}
    </div>
  );
}

/** The error as the server reported it, for the AI to fix the statement. */
function describeError(error: unknown, t: Messages) {
  if (!isAppError(error) || error.kind !== "database") return errorMessage(error, t);
  return [
    error.message,
    `SQLSTATE ${error.code}`,
    error.detail && `DETAIL: ${error.detail}`,
    error.hint && `HINT: ${error.hint}`,
    error.position && `POSITION: character ${error.position} of the statement`,
  ]
    .filter(Boolean)
    .join("\n");
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
