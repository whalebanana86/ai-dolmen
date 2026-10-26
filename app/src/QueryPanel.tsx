import { KeyboardEvent, useState } from "react";
import { SessionInfo, StatementResult, errorMessage, isDbError, pg } from "./api";
import { ResultView } from "./ResultView";

type Outcome =
  | { kind: "results"; results: StatementResult[]; elapsedMs: number }
  | { kind: "error"; error: unknown; sql: string };

interface Props {
  session: SessionInfo;
  onDisconnect: () => void;
}

export function QueryPanel({ session, onDisconnect }: Props) {
  const [sql, setSql] = useState("SELECT version();");
  const [running, setRunning] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);

  async function run() {
    if (running || !sql.trim()) return;
    setRunning(true);
    const started = performance.now();
    try {
      const results = await pg.execute(session.id, sql);
      setOutcome({ kind: "results", results, elapsedMs: performance.now() - started });
    } catch (error) {
      setOutcome({ kind: "error", error, sql });
    } finally {
      setRunning(false);
    }
  }

  function cancel() {
    // The running execute rejects with query_canceled, which shows as its outcome.
    pg.cancel(session.id).catch((error) => console.error("cancel failed", error));
  }

  async function disconnect() {
    await pg.disconnect(session.id);
    onDisconnect();
  }

  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
      event.preventDefault();
      run();
    }
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
          <button className="primary" onClick={run} disabled={!sql.trim()} title="⌘/Ctrl + Enter">
            Run
          </button>
        )}
        <button onClick={disconnect}>Disconnect</button>
      </div>

      <textarea
        className="editor"
        value={sql}
        onChange={(e) => setSql(e.target.value)}
        onKeyDown={onKeyDown}
        spellCheck={false}
        autoFocus
      />

      <section className="results">
        {running && <p className="muted status">Running…</p>}
        {!running && outcome?.kind === "error" && <ErrorView error={outcome.error} sql={outcome.sql} />}
        {!running && outcome?.kind === "results" && (
          <>
            <p className="muted status">
              {outcome.results.length} {outcome.results.length === 1 ? "statement" : "statements"} ·{" "}
              {Math.round(outcome.elapsedMs)} ms
            </p>
            {outcome.results.map((result, index) => (
              <ResultView key={index} index={index} result={result} />
            ))}
          </>
        )}
      </section>
    </main>
  );
}

function ErrorView({ error, sql }: { error: unknown; sql: string }) {
  const db = isDbError(error) && error.kind === "database" ? error : null;
  const location = db?.position ? lineAndColumn(sql, db.position) : null;

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

/** PostgreSQL counts positions in characters, so split by code point rather than UTF-16 unit. */
function lineAndColumn(sql: string, position: number) {
  const lines = [...sql].slice(0, position - 1).join("").split("\n");
  return { line: lines.length, column: [...lines[lines.length - 1]].length + 1 };
}
