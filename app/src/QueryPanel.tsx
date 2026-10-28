import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { EditorView } from "@codemirror/view";
import {
  DataSource,
  ExplainOutput,
  Introspection,
  ObjectRef,
  SavedConnection,
  SnapshotRemote,
  SchemaObject,
  Table,
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
import { ActivityBar, BranchIcon, DatabaseIcon, DiagramIcon, GearIcon, HistoryIcon, ResultsIcon } from "./ActivityBar";
import { AiPanel, FixTarget, useAi } from "./AiPanel";
import { ContextMenu } from "./ContextMenu";
import { AI_ENABLED } from "./features";
import { EnvironmentBadge } from "./ConnectionForm";
import { ExportDialog } from "./ExportDialog";
import { ImportDialog } from "./ImportDialog";
import { CreatePanel, DetailPanel } from "./DetailPanel";
import { ErrorBoundary } from "./ErrorBoundary";
import { HistoryPanel } from "./HistoryPanel";
import { defaultLayout, useLayout } from "./layout";
import { Splitter } from "./Splitter";
import { DiscardChanges, ResultView, SaveTarget, StatementLine, resultCount } from "./ResultView";
import { NO_CHANGES, PendingRows, changeCount } from "./rowEdits";
import { PlanView } from "./PlanView";
import { RiskDialog, RiskyStatement } from "./RiskDialog";
import { SchemaTree } from "./SchemaTree";
import { SettingsDialog } from "./SettingsDialog";
import { SnapshotPanel } from "./SnapshotPanel";
import { SqlEditor } from "./SqlEditor";
import { SqlTabs } from "./SqlTabs";
import { quoteIdent, statementTable } from "./sqlIdent";
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
  /** Results of the statements that succeeded, in order, with changes to their rows not saved yet. */
  results: { result: StatementResult; statement: FixTarget; pending?: PendingRows }[];
  /** The error that stopped the run, if any, and the statement that failed. */
  failure: { error: unknown; location: Location | null; statement: FixTarget } | null;
  statementCount: number;
  firstLine: number;
  elapsedMs: number;
}

/** A statement's plan, shown in place of results. */
interface Explained {
  runId: number;
  statement: FixTarget;
  /** Measured by running it, rather than estimated. */
  analyzed: boolean;
  changesData: boolean;
  output: ExplainOutput | null;
  failure: { error: unknown; location: Location | null } | null;
}

/** An editor tab: a console with its own session, text and results. */
interface SqlTab {
  id: number;
  title: string;
  /** Its own session, opened the first time it's needed; the first tab uses the connection's. */
  sessionId: number | null;
  /** The schema its unqualified names resolve in, as last read or chosen. */
  schema: string | null;
  /** The text the tab started with. */
  initialSql: string;
  outcome: Outcome | null;
  plan: Explained | null;
  running: boolean;
  hasText: boolean;
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
  // Whether the connection's sessions refuse writes; every tab's session follows it.
  const [readOnly, setReadOnly] = useState(session.readOnly);
  const readOnlyRef = useRef(readOnly);
  readOnlyRef.current = readOnly;
  const [confirmingWritable, setConfirmingWritable] = useState(false);
  const [lockBusy, setLockBusy] = useState(false);
  // The export or import dialog, while open.
  const [transfer, setTransfer] = useState<
    | { kind: "export"; sessionId: number; source: DataSource; label: string; fileName: string; insertTable: string }
    | { kind: "import"; schema: string; table: Table }
    | null
  >(null);
  // Risky statements waiting for the user to say whether to run them.
  const [risky, setRisky] = useState<{ statements: RiskyStatement[]; decide: (run: boolean) => void } | null>(null);
  // Unsaved changes to result rows, waiting for the user to say whether to drop them.
  const [discarding, setDiscarding] = useState<{ count: number; decide: (drop: boolean) => void } | null>(null);
  // Editor tabs, each a console with its own session, kept per connection.
  const editorKey = session.savedId ?? session.label;
  const [tabs, setTabs] = useState<SqlTab[] | null>(null);
  const [activeId, setActiveId] = useState(1);
  const tabsRef = useRef<SqlTab[] | null>(null);
  tabsRef.current = tabs;
  const activeIdRef = useRef(activeId);
  activeIdRef.current = activeId;
  const nextTabId = useRef(1);
  const views = useRef(new Map<number, EditorView>());
  const editorRefs = useRef(new Map<number, { current: EditorView | null }>());
  /** Each tab's text as last typed, for saving even after its editor is gone. */
  const texts = useRef(new Map<number, string>());
  const forking = useRef(new Map<number, Promise<number>>());
  const saveTimer = useRef<number | undefined>(undefined);
  /** The active tab's editor, for everything that works with "the editor". */
  const viewRef = useMemo(
    () => ({
      get current() {
        return views.current.get(activeIdRef.current) ?? null;
      },
      set current(_view: EditorView | null) {},
    }),
    [],
  );
  const active = tabs?.find((tab) => tab.id === activeId) ?? null;
  const running = active?.running ?? false;
  const outcome = active?.outcome ?? null;
  const hasText = active?.hasText ?? false;
  const updateTab = useCallback((id: number, change: (tab: SqlTab) => Partial<SqlTab>) => {
    setTabs((current) => current?.map((tab) => (tab.id === id ? { ...tab, ...change(tab) } : tab)) ?? current);
  }, []);
  const [historyOpen, setHistoryOpen] = useState(false);
  // The connection's menu, under its name, while open.
  const [connectionMenu, setConnectionMenu] = useState<{ x: number; y: number } | null>(null);
  const [introspection, setIntrospection] = useState<Introspection | null>(null);
  const [loadingSchema, setLoadingSchema] = useState(false);
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
        const restored = savedTabs(saved, tRef.current);
        const made = restored.tabs.map(
          (tab, i): SqlTab => ({
            id: i + 1,
            title: tab.title,
            sessionId: i === 0 ? session.id : null,
            schema: tab.schema,
            initialSql: tab.sql,
            outcome: null,
            plan: null,
            running: false,
            hasText: tab.sql.trim() !== "",
          }),
        );
        for (const tab of made) texts.current.set(tab.id, tab.initialSql);
        nextTabId.current = made.length + 1;
        setTabs(made);
        setActiveId(made[Math.min(Math.max(restored.active, 0), made.length - 1)].id);
        // The first tab's schema is the connection's own session's.
        const first = made[0];
        (first.schema ? pg.setSchema(session.id, first.schema) : pg.currentSchema(session.id))
          .then((schema) => updateTab(first.id, () => ({ schema })))
          .catch(() => {});
      });
  }, [editorKey]);

  /** Saves the tabs now, instead of after the pause. */
  const saveTabs = useCallback(() => {
    clearTimeout(saveTimer.current);
    saveTimer.current = undefined;
    const list = tabsRef.current;
    if (!list) return;
    const saved = {
      version: 1,
      active: Math.max(0, list.findIndex((tab) => tab.id === activeIdRef.current)),
      tabs: list.map((tab) => ({
        title: tab.title,
        sql: texts.current.get(tab.id) ?? tab.initialSql,
        schema: tab.schema,
      })),
    };
    history.saveEditor(editorKey, JSON.stringify(saved)).catch((error) => console.error("saving the editor failed", error));
  }, [editorKey]);
  useEffect(() => saveTabs, [saveTabs]);
  const scheduleSave = useCallback(() => {
    clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(saveTabs, 800);
  }, [saveTabs]);

  // Tabs opened, closed, renamed, switched or moved to another schema are kept too.
  const tabsShape = JSON.stringify([activeId, tabs?.map((tab) => [tab.id, tab.title, tab.schema])]);
  useEffect(() => {
    if (tabsRef.current) scheduleSave();
  }, [tabsShape]);

  function editorChanged(id: number, value: string) {
    texts.current.set(id, value);
    const hasText = value.trim() !== "";
    if (tabsRef.current?.find((tab) => tab.id === id)?.hasText !== hasText) updateTab(id, () => ({ hasText }));
    scheduleSave();
  }

  /** Where tab `id`'s editor registers itself. */
  function editorRef(id: number) {
    let ref = editorRefs.current.get(id);
    if (!ref) {
      ref = {
        get current() {
          return views.current.get(id) ?? null;
        },
        set current(view: EditorView | null) {
          if (view) views.current.set(id, view);
          else views.current.delete(id);
        },
      };
      editorRefs.current.set(id, ref);
    }
    return ref;
  }

  /** The tab's session, opened the first time the tab needs one. */
  function sessionOf(id: number): Promise<number> {
    const tab = tabsRef.current?.find((t) => t.id === id);
    if (!tab) return Promise.reject(new Error("the tab is closed"));
    if (tab.sessionId !== null) return Promise.resolve(tab.sessionId);
    let opening = forking.current.get(id);
    if (!opening) {
      opening = (async () => {
        const forked = await pg.fork(session.id);
        const schema = tab.schema
          ? await pg.setSchema(forked.id, tab.schema).catch(() => null)
          : await pg.currentSchema(forked.id).catch(() => null);
        updateTab(id, () => ({ sessionId: forked.id, schema }));
        return forked.id;
      })();
      forking.current.set(id, opening);
      opening.finally(() => forking.current.delete(id)).catch(() => {});
    }
    return opening;
  }

  function newTab() {
    const id = nextTabId.current++;
    texts.current.set(id, "");
    const schema = tabsRef.current?.find((tab) => tab.id === activeIdRef.current)?.schema ?? null;
    setTabs((list) => [
      ...(list ?? []),
      {
        id,
        title: t.tabs.console(id),
        sessionId: null,
        schema,
        initialSql: "",
        outcome: null,
        plan: null,
        running: false,
        hasText: false,
      },
    ]);
    setActiveId(id);
  }

  function closeTab(id: number) {
    const list = tabsRef.current ?? [];
    if (list.length <= 1) return;
    const index = list.findIndex((tab) => tab.id === id);
    const tab = list[index];
    if (tab.sessionId !== null && tab.sessionId !== session.id) {
      pg.disconnect(tab.sessionId).catch((error) => console.error("closing the tab's session failed", error));
    }
    texts.current.delete(id);
    editorRefs.current.delete(id);
    const rest = list.filter((t) => t.id !== id);
    setTabs(rest);
    if (activeIdRef.current === id) setActiveId(rest[Math.min(index, rest.length - 1)].id);
  }

  function switchTab(step: number) {
    const list = tabsRef.current ?? [];
    const index = list.findIndex((tab) => tab.id === activeIdRef.current);
    if (list.length > 1) setActiveId(list[(index + step + list.length) % list.length].id);
  }

  async function changeSchema(schema: string) {
    const id = activeIdRef.current;
    try {
      const now = await pg.setSchema(await sessionOf(id), schema);
      updateTab(id, () => ({ schema: now }));
    } catch (error) {
      updateTab(id, () => ({
        plan: null,
        outcome: {
          runId: ++runCount.current,
          results: [],
          failure: { error, location: null, statement: { statement: `SET search_path TO ${schema}`, start: 0, end: 0 } },
          statementCount: 1,
          firstLine: 1,
          elapsedMs: 0,
        },
      }));
    }
  }

  // The keyboard reaches the latest tab functions through here.
  const tabActions = useRef({ newTab, switchTab });
  tabActions.current = { newTab, switchTab };

  // ⌘B and ⌘J show and hide the schema tree and the results, as in VS Code; ⌘⇧H opens the history.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && !e.altKey && e.key.toLowerCase() === "h") {
        e.preventDefault();
        setHistoryOpen((open) => !open);
        return;
      }
      // ⌘T opens a tab; Ctrl+Tab and Ctrl+Shift+Tab go through them.
      if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "t") {
        e.preventDefault();
        tabActions.current.newTab();
        return;
      }
      if (e.ctrlKey && e.key === "Tab") {
        e.preventDefault();
        tabActions.current.switchTab(e.shiftKey ? -1 : 1);
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

  /** Asks before replacing results whose rows have unsaved changes; true to go on. */
  function mayReplaceResults(tab: SqlTab) {
    const count = (tab.outcome?.results ?? []).reduce(
      (sum, { pending }) => sum + (pending ? changeCount(pending) : 0),
      0,
    );
    if (count === 0) return Promise.resolve(true);
    return new Promise<boolean>((decide) => setDiscarding({ count, decide }));
  }

  /** Applies a change to result `index` of a run, if the tab still shows that run. */
  function changeResult(
    tabId: number,
    runId: number,
    index: number,
    change: { result?: StatementResult; pending?: PendingRows },
  ) {
    updateTab(tabId, (tab) =>
      tab.outcome?.runId !== runId
        ? {}
        : {
            outcome: {
              ...tab.outcome,
              results: tab.outcome.results.map((entry, i) => (i === index ? { ...entry, ...change } : entry)),
            },
          },
    );
  }

  async function run() {
    const view = viewRef.current;
    const tabId = activeIdRef.current;
    const tab = tabsRef.current?.find((t) => t.id === tabId);
    if (!view || !tab || tab.running) return;
    if (!(await mayReplaceResults(tab))) return view.focus();
    const sql = view.state.doc.toString();
    const { from, to } = view.state.selection.main;
    const ranges = await statementsToRun(sql, from, to);
    if (ranges.length === 0) return;

    updateTab(tabId, () => ({ running: true }));
    updateLayout({ resultsVisible: true });
    const started = performance.now();
    const results: Outcome["results"] = [];
    let failure: Outcome["failure"] = null;
    let sessionId: number | null = null;
    try {
      sessionId = await sessionOf(tabId);
    } catch (error) {
      const first = ranges[0];
      failure = {
        error,
        location: null,
        statement: { statement: sql.slice(first.start, first.end), start: first.start, end: first.end },
      };
    }
    // Ask before statements that change every row or drop something. A
    // read-only session refuses them anyway.
    if (sessionId !== null && !readOnlyRef.current) {
      const statements = ranges.map((range) => sql.slice(range.start, range.end));
      let proceed = false;
      try {
        const found = (await pg.assess(sessionId, statements)).flatMap((assessment, i) =>
          assessment ? [{ statement: statements[i], assessment }] : [],
        );
        proceed =
          found.length === 0 ||
          (await new Promise<boolean>((decide) => setRisky({ statements: found, decide })));
      } catch (error) {
        const first = ranges[0];
        failure = { error, location: null, statement: { statement: statements[0], start: first.start, end: first.end } };
      }
      if (!proceed && failure === null) {
        updateTab(tabId, () => ({ running: false }));
        view.focus();
        return;
      }
      if (!proceed) sessionId = null;
    }
    // One statement at a time, so a failure doesn't roll back the ones before it.
    for (const range of sessionId === null ? [] : ranges) {
      const statement = sql.slice(range.start, range.end);
      try {
        for (const result of await pg.execute(sessionId!, statement)) {
          results.push({ result, statement: { statement, start: range.start, end: range.end } });
        }
      } catch (error) {
        failure = {
          error,
          location: errorLocation(sql, range.start, statement, error),
          statement: { statement, start: range.start, end: range.end },
        };
        break;
      }
    }
    const finished: Outcome = {
      runId: ++runCount.current,
      results,
      failure,
      statementCount: ranges.length,
      firstLine: lineAt(sql, ranges[0].start),
      elapsedMs: performance.now() - started,
    };
    updateTab(tabId, () => ({ running: false, outcome: finished, plan: null }));
    // The statements may have changed the search path.
    if (sessionId !== null) {
      pg.currentSchema(sessionId)
        .then((schema) => updateTab(tabId, () => ({ schema })))
        .catch(() => {});
    }
    // DDL such as CREATE TABLE returns no rows; reload the tree, completion and snapshot status.
    if (results.some(({ result }) => result.kind === "command")) {
      refreshSchema();
      refreshSnapshot();
    }
  }

  /**
   * Shows the plan of the statement at the cursor (the first one selected), or
   * of `target` again. Measuring a statement that changes data waits for the
   * plan's own button, which says it rolls back.
   */
  async function explain(analyze: boolean, target?: FixTarget) {
    const view = viewRef.current;
    const tabId = activeIdRef.current;
    const tab = tabsRef.current?.find((t) => t.id === tabId);
    if (!view || !tab || tab.running) return;
    if (!(await mayReplaceResults(tab))) return view.focus();
    let statement = target;
    if (!statement) {
      const sql = view.state.doc.toString();
      const { from, to } = view.state.selection.main;
      const [range] = await statementsToRun(sql, from, to);
      if (!range) return;
      statement = { statement: sql.slice(range.start, range.end), start: range.start, end: range.end };
    }
    const changes = changesData(statement.statement);
    const measure = analyze && (!changes || target !== undefined);
    updateTab(tabId, () => ({ running: true }));
    updateLayout({ resultsVisible: true });
    let output: ExplainOutput | null = null;
    let failure: Explained["failure"] = null;
    try {
      output = await pg.explain(await sessionOf(tabId), statement.statement, measure);
    } catch (error) {
      failure = {
        error,
        location: errorLocation(view.state.doc.toString(), statement.start, statement.statement, error),
      };
    }
    const plan: Explained = {
      runId: ++runCount.current,
      statement,
      analyzed: measure,
      changesData: changes,
      output,
      failure,
    };
    updateTab(tabId, () => ({ running: false, outcome: null, plan }));
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

  /** Selects a statement that ran, where it is in the editor now. */
  function revealStatement(target: FixTarget) {
    const view = viewRef.current;
    if (!view) return;
    let start = target.start;
    if (view.state.sliceDoc(target.start, target.end) !== target.statement) {
      start = view.state.doc.toString().indexOf(target.statement);
      if (start < 0) return;
    }
    view.dispatch({
      selection: { anchor: start, head: start + target.statement.length },
      scrollIntoView: true,
    });
    view.focus();
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
    if (active?.sessionId != null) {
      pg.cancel(active.sessionId).catch((error) => console.error("cancel failed", error));
    }
  }

  /** Opens the export of a statement's rows, run again on the active tab's session. */
  function exportResult(statement: string) {
    const table = statementTable(statement);
    setTransfer({
      kind: "export",
      sessionId: active?.sessionId ?? session.id,
      source: { kind: "query", sql: statement },
      label: table ?? t.transfer.queryResults,
      fileName: table?.split(".").pop()?.replace(/"/g, "") ?? "result",
      insertTable: table ?? "result",
    });
  }

  /** Locks or unlocks writing; unlocking production takes a second click. */
  async function toggleReadOnly() {
    const next = !readOnly;
    if (!next && session.environment === "production" && !confirmingWritable) {
      setConfirmingWritable(true);
      return;
    }
    setConfirmingWritable(false);
    setLockBusy(true);
    try {
      await pg.setReadOnly(session.id, next);
      setReadOnly(next);
    } catch (error) {
      console.error("changing read-only failed", error);
    } finally {
      setLockBusy(false);
    }
  }

  async function disconnect() {
    saveTabs();
    for (const tab of tabsRef.current ?? []) {
      if (tab.sessionId !== null && tab.sessionId !== session.id) pg.disconnect(tab.sessionId).catch(() => {});
    }
    await pg.disconnect(session.id);
    onDisconnect();
  }

  return (
    <main className={session.environment ? `workspace environment-${session.environment}` : "workspace"}>
      <div className="toolbar">
        <button
          className="connection-button"
          // A click on the open menu's button closes it, rather than closing and opening it again.
          onMouseDown={(e) => connectionMenu && e.stopPropagation()}
          onClick={(e) => {
            const box = e.currentTarget.getBoundingClientRect();
            setConnectionMenu(connectionMenu ? null : { x: box.left, y: box.bottom + 4 });
          }}
          title={session.address}
          aria-haspopup="menu"
          aria-expanded={!!connectionMenu}
        >
          {session.environment && <EnvironmentBadge environment={session.environment} />}
          <span className="toolbar-label">{session.label}</span>
          <span className="muted" aria-hidden="true">
            ▾
          </span>
        </button>
        <button
          className={`lock-toggle ${readOnly ? "locked" : "unlocked"}`}
          onClick={toggleReadOnly}
          onBlur={() => setConfirmingWritable(false)}
          disabled={lockBusy}
          aria-pressed={readOnly}
          title={readOnly ? t.workspace.readOnlyTitle : t.workspace.writableTitle}
        >
          <LockIcon locked={readOnly} />
          {confirmingWritable ? t.workspace.confirmWritable : readOnly ? t.workspace.readOnly : t.workspace.writable}
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
        <button onClick={() => explain(false)} disabled={!hasText || running} title={t.plan.buttonTitle}>
          {t.plan.button}
        </button>
        {AI_ENABLED && (
          <button onClick={() => setAiPrompting(true)} title={t.ai.buttonTitle}>
            {t.ai.button}
          </button>
        )}
      </div>
      {connectionMenu && (
        <ContextMenu
          x={connectionMenu.x}
          y={connectionMenu.y}
          items={[
            { note: session.address },
            { note: `PostgreSQL ${session.serverVersion}` },
            { separator: true },
            { label: t.workspace.disconnect, onSelect: disconnect },
          ]}
          onClose={() => setConnectionMenu(null)}
        />
      )}

      <div className="workspace-body">
        <ActivityBar
          label={t.workspace.panels}
          top={[
            {
              key: "tree",
              label: t.workspace.tree,
              title: t.workspace.toggleTree(layout.treeVisible),
              icon: <DatabaseIcon />,
              pressed: layout.treeVisible,
              onClick: () => togglePanel("treeVisible"),
            },
            {
              key: "history",
              label: t.history.button,
              title: t.history.buttonTitle,
              icon: <HistoryIcon />,
              pressed: historyOpen,
              onClick: () => setHistoryOpen(true),
            },
            {
              key: "erd",
              label: t.workspace.erd,
              title: t.workspace.erdTitle,
              icon: <DiagramIcon />,
              pressed: erd !== null,
              disabled: !introspection,
              onClick: () => setErd({ schema: introspection?.defaultSchema ?? null, focus: null }),
            },
            {
              key: "snapshot",
              label: t.workspace.snapshot,
              title: snapshotStatus
                ? `${t.workspace.snapshotTitle} · ${t.workspace.snapshotChanges(snapshotStatus.changes.length)}`
                : t.workspace.snapshotTitle,
              icon: <BranchIcon />,
              pressed: snapshotOpen,
              badge: snapshotStatus?.changes.length,
              onClick: () => setSnapshotOpen(true),
            },
          ]}
          bottom={[
            {
              key: "results",
              label: t.workspace.toggleResults(layout.resultsVisible),
              icon: <ResultsIcon shown={layout.resultsVisible} />,
              pressed: layout.resultsVisible,
              onClick: () => togglePanel("resultsVisible"),
            },
            {
              key: "settings",
              label: t.common.settings,
              icon: <GearIcon />,
              onClick: () => setSettingsOpen(true),
            },
          ]}
        />
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
                exportData: (schema, relation) =>
                  setTransfer({
                    kind: "export",
                    sessionId: session.id,
                    source: { kind: "table", schema, name: relation },
                    label: `${schema}.${relation}`,
                    fileName: relation,
                    insertTable: `${quoteIdent(schema)}.${quoteIdent(relation)}`,
                  }),
                importData: (schema, table) => setTransfer({ kind: "import", schema, table }),
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
          {tabs && (
            <SqlTabs
              tabs={tabs}
              activeId={activeId}
              onSelect={setActiveId}
              onNew={newTab}
              onClose={closeTab}
              onRename={(id, title) => updateTab(id, () => ({ title }))}
              schemas={introspection?.database.schemas.map((schema) => schema.name) ?? []}
              schema={active?.schema ?? null}
              onSchemaChange={changeSchema}
            />
          )}
          {tabs === null ? (
            // The saved tabs load in a moment; the editors are made with their text.
            <div
              className={layout.resultsVisible ? "editor" : "editor fill"}
              style={layout.resultsVisible ? { height: layout.editorHeight } : undefined}
            />
          ) : (
            tabs.map((tab) => (
              <SqlEditor
                key={tab.id}
                initialValue={tab.initialSql}
                introspection={introspection}
                onRun={run}
                onAsk={AI_ENABLED ? () => setAiPrompting(true) : undefined}
                onExplain={explain}
                onChange={(value) => editorChanged(tab.id, value)}
                viewRef={editorRef(tab.id)}
                height={layout.resultsVisible ? layout.editorHeight : null}
                defaultSchema={tab.schema}
                hidden={tab.id !== activeId}
              />
            ))
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

          {AI_ENABLED && (aiPrompting || conversation) && (
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
                <RunResults
                  key={outcome.runId}
                  outcome={outcome}
                  target={
                    active?.sessionId != null
                      ? {
                          sessionId: active.sessionId,
                          readOnly,
                          label: session.label,
                          address: session.address,
                          environment: session.environment,
                        }
                      : null
                  }
                  onChange={(index, change) => changeResult(active!.id, outcome.runId, index, change)}
                  onReveal={revealStatement}
                  onExport={exportResult}
                  onFix={AI_ENABLED ? fixWithAi : undefined}
                />
              )}
              {!running && active?.plan?.output && (
                <PlanView
                  key={active.plan.runId}
                  output={active.plan.output}
                  analyzed={active.plan.analyzed}
                  statement={active.plan.statement.statement}
                  changesData={active.plan.changesData}
                  onMeasure={() => explain(true, active.plan!.statement)}
                  onReveal={() => revealStatement(active.plan!.statement)}
                />
              )}
              {!running && active?.plan?.failure && (
                <ErrorView
                  {...active.plan.failure}
                  statement={active.plan.statement}
                  onFix={
                    AI_ENABLED
                      ? () => fixWithAi({ ...active.plan!.failure!, statement: active.plan!.statement })
                      : undefined
                  }
                  onReveal={() => revealStatement(active.plan!.statement)}
                />
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
      {transfer?.kind === "export" && <ExportDialog {...transfer} onClose={() => setTransfer(null)} />}
      {transfer?.kind === "import" && (
        <ImportDialog
          sessionId={session.id}
          schema={transfer.schema}
          table={transfer.table}
          onClose={() => setTransfer(null)}
        />
      )}
      {discarding && (
        <DiscardChanges
          count={discarding.count}
          onDiscard={() => {
            discarding.decide(true);
            setDiscarding(null);
          }}
          onCancel={() => {
            discarding.decide(false);
            setDiscarding(null);
          }}
        />
      )}
      {risky && (
        <RiskDialog
          risky={risky.statements}
          target={{ label: session.label, address: session.address, environment: session.environment }}
          onRun={() => {
            risky.decide(true);
            setRisky(null);
          }}
          onCancel={() => {
            risky.decide(false);
            setRisky(null);
          }}
        />
      )}
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

/** A padlock, closed while the connection is read-only. */
function LockIcon({ locked }: { locked: boolean }) {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true">
      <rect x="3" y="7" width="10" height="7.5" rx="1.5" fill="currentColor" />
      <path
        d={locked ? "M5 7V5a3 3 0 0 1 6 0v2" : "M5 7V5a3 3 0 0 1 5.8-1"}
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
      />
    </svg>
  );
}


interface SavedTabs {
  active: number;
  tabs: { title: string; sql: string; schema: string | null }[];
}

/** The tabs saved for a connection: this version's JSON, or the text of the single editor before tabs. */
function savedTabs(saved: string | null, t: Messages): SavedTabs {
  if (saved?.startsWith("{")) {
    try {
      const parsed = JSON.parse(saved) as { active?: unknown; tabs?: unknown };
      if (Array.isArray(parsed.tabs) && parsed.tabs.length > 0) {
        return {
          active: typeof parsed.active === "number" ? parsed.active : 0,
          tabs: parsed.tabs.map((tab: { title?: unknown; sql?: unknown; schema?: unknown }, i: number) => ({
            title: typeof tab.title === "string" && tab.title ? tab.title : t.tabs.console(i + 1),
            sql: typeof tab.sql === "string" ? tab.sql : "",
            schema: typeof tab.schema === "string" ? tab.schema : null,
          })),
        };
      }
    } catch {
      // Text that only looks like JSON: an editor's text from before tabs.
    }
  }
  return { active: 0, tabs: [{ title: t.tabs.console(1), sql: saved ?? INITIAL_SQL, schema: null }] };
}

/** Whether running the statement may change data, judged by how it starts. */
function changesData(statement: string) {
  const text = statement.replace(/--[^\n]*|\/\*[\s\S]*?\*\//g, " ").trim();
  const first = /^\(*\s*(\w+)/.exec(text)?.[1]?.toLowerCase() ?? "";
  if (["select", "values", "table", "show"].includes(first)) return false;
  // A WITH query can hold an INSERT, UPDATE or DELETE.
  if (first === "with") return /\b(insert|update|delete|merge)\b/i.test(text);
  return true;
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

/**
 * A run's results. Several are tabs, so the one shown fills the panel and only
 * its grid scrolls; the error that stopped the run is the last tab, shown first.
 */
function RunResults({
  outcome,
  target,
  onChange,
  onReveal,
  onExport,
  onFix,
}: {
  outcome: Outcome;
  /** Where changes to the rows are saved: the session the statements ran in. */
  target: SaveTarget | null;
  onChange: (index: number, change: { result?: StatementResult; pending?: PendingRows }) => void;
  onReveal: (statement: FixTarget) => void;
  onExport: (statement: string) => void;
  onFix?: (failure: NonNullable<Outcome["failure"]>) => void;
}) {
  const t = useT();
  const { results, failure } = outcome;
  const [selected, setSelected] = useState(failure ? results.length : Math.max(0, results.length - 1));
  const shown = results[selected];
  // A single result's header says what it is; several get a summary and tabs.
  const single = results.length + (failure ? 1 : 0) === 1;

  return (
    <>
      {!single && (
        <p className="muted status">
          {t.workspace.runStatus(
            outcome.firstLine,
            results.length,
            outcome.statementCount,
            Math.round(outcome.elapsedMs),
          )}
        </p>
      )}
      {results.length + (failure ? 1 : 0) > 1 && (
        <div className="result-tabs" role="tablist" aria-label={t.results.tabs}>
          {results.map(({ result, statement, pending }, i) => {
            const table = statementTable(statement.statement);
            const unsaved = pending ? changeCount(pending) : 0;
            return (
              <button
                key={i}
                role="tab"
                aria-selected={i === selected}
                className={i === selected ? "result-tab selected" : "result-tab"}
                onClick={() => setSelected(i)}
                title={statement.statement}
              >
                {t.results.statement(i + 1)}
                {table && (
                  <>
                    {" · "}
                    <strong className="result-table">{table}</strong>
                  </>
                )}
                <span className="muted"> · {resultCount(result, t)}</span>
                {unsaved > 0 && (
                  <span className="result-unsaved" title={t.rows.unsaved(unsaved)}>
                    {" "}
                    ●
                  </span>
                )}
              </button>
            );
          })}
          {failure && (
            <button
              role="tab"
              aria-selected={selected === results.length}
              className={selected === results.length ? "result-tab error selected" : "result-tab error"}
              onClick={() => setSelected(results.length)}
              title={failure.statement.statement}
            >
              {t.results.error(results.length + 1)}
            </button>
          )}
        </div>
      )}
      {shown ? (
        <ResultView
          key={selected}
          result={shown.result}
          elapsedMs={single ? outcome.elapsedMs : null}
          sql={shown.statement.statement}
          pending={shown.pending ?? NO_CHANGES}
          target={target}
          onChange={(change) => onChange(selected, change)}
          onReveal={() => onReveal(shown.statement)}
          onExport={() => onExport(shown.statement.statement)}
        />
      ) : (
        failure && (
          <ErrorView
            {...failure}
            onFix={onFix && (() => onFix(failure))}
            onReveal={() => onReveal(failure.statement)}
          />
        )
      )}
    </>
  );
}

function ErrorView({
  error,
  location,
  statement,
  onFix,
  onReveal,
}: {
  error: unknown;
  location: Location | null;
  statement: FixTarget;
  /** Offers to fix the statement with AI. */
  onFix?: () => void;
  onReveal: () => void;
}) {
  const t = useT();
  const db = isAppError(error) && error.kind === "database" ? error : null;

  return (
    <div className="error-box">
      <StatementLine sql={statement.statement} onReveal={onReveal} />
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
      {/* read_only_sql_transaction */}
      {db?.code === "25006" && <p>{t.workspace.readOnlyHint}</p>}
      {/* Only the server's errors are about the SQL; connection problems,
          cancelling and the read-only lock aren't. */}
      {onFix && db && db.code !== "57014" && db.code !== "25006" && (
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
