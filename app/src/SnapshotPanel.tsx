import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { FileChange, SnapshotStatus, errorMessage, snapshots } from "./api";

interface Props {
  sessionId: number;
  /** The database name, for the default commit message. */
  database: string;
  dir: string | null;
  onChooseDir: (dir: string) => void;
  status: SnapshotStatus | null;
  statusError: string | null;
  checking: boolean;
  onRefresh: () => void;
  onClose: () => void;
}

const KIND_LETTERS = { added: "A", modified: "M", removed: "D" } as const;

/**
 * Compares the database with its snapshot folder and writes the snapshot,
 * optionally committing it to the folder's Git repository.
 */
export function SnapshotPanel({
  sessionId,
  database,
  dir,
  onChooseDir,
  status,
  statusError,
  checking,
  onRefresh,
  onClose,
}: Props) {
  const changes = status?.changes ?? [];
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [message, setMessage] = useState("");
  const [messageEdited, setMessageEdited] = useState(false);
  const [saving, setSaving] = useState(false);
  const [result, setResult] = useState<{ text: string; error: boolean } | null>(null);

  // Follow the changes with the default message until the user edits it.
  useEffect(() => {
    if (!messageEdited) setMessage(defaultMessage(database, changes));
  }, [status, database, messageEdited]);

  async function chooseDir() {
    const picked = await open({ directory: true, title: "Choose the schema snapshot folder" });
    if (typeof picked === "string") {
      setResult(null);
      onChooseDir(picked);
    }
  }

  async function save(commit: boolean) {
    if (!dir) return;
    setSaving(true);
    setResult(null);
    try {
      const saved = await snapshots.save(sessionId, dir, commit ? message.trim() : null);
      const count = `${changes.length} ${changes.length === 1 ? "file" : "files"}`;
      setResult({
        text: saved.commit ? `Committed ${saved.commit.slice(0, 7)} (${count})` : `Wrote ${count}`,
        error: false,
      });
      setMessageEdited(false);
      onRefresh();
    } catch (error) {
      setResult({ text: errorMessage(error), error: true });
    } finally {
      setSaving(false);
    }
  }

  function toggle(path: string) {
    setExpanded((current) => {
      const next = new Set(current);
      if (!next.delete(path)) next.add(path);
      return next;
    });
  }

  return (
    <div className="panel-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <aside className="side-panel" role="dialog" aria-label="Schema snapshot">
        <header className="side-panel-header">
          <strong>Schema snapshot</strong>
          <span className="spacer" />
          <button onClick={onRefresh} disabled={!dir || checking} title="Compare again">
            ↻
          </button>
          <button onClick={onClose} title="Close">
            ✕
          </button>
        </header>

        <section className="side-panel-section">
          {dir ? (
            <div className="snapshot-folder">
              <code title={dir}>{dir}</code>
              <button onClick={chooseDir}>Change…</button>
            </div>
          ) : (
            <>
              <p>
                Choose a folder for this database's schema snapshot: one DDL file per table, view and
                function, ready to commit. A folder in your app's repository, such as{" "}
                <code>db/schema</code>, works well.
              </p>
              <button className="primary" onClick={chooseDir}>
                Choose folder…
              </button>
            </>
          )}
          {dir && status && (
            <p className="muted">
              {status.repository
                ? `Git: ${status.repository.root}${status.repository.branch ? ` · ${status.repository.branch}` : ""}`
                : "Not inside a Git repository: the files can be written but not committed."}
            </p>
          )}
          {dir && status && !status.initialized && (
            <p className="muted">The first write sets this folder up as a snapshot folder.</p>
          )}
        </section>

        {dir && (
          <section className="side-panel-section grow">
            {statusError && <p className="error-text">{statusError}</p>}
            {!status && checking && <p className="muted">Comparing with the database…</p>}
            {status && changes.length === 0 && <p className="muted">The snapshot matches the database.</p>}
            {changes.length > 0 && (
              <>
                <p className="muted">
                  {changes.length} {changes.length === 1 ? "change" : "changes"} since the last snapshot
                </p>
                <ul className="change-list">
                  {changes.map((change) => (
                    <li key={change.path}>
                      <button className="change-row" onClick={() => toggle(change.path)}>
                        <span className={`change-kind ${change.kind}`}>{KIND_LETTERS[change.kind]}</span>
                        <span className="change-path">{change.path}</span>
                      </button>
                      {expanded.has(change.path) && <Diff text={change.diff} />}
                    </li>
                  ))}
                </ul>
              </>
            )}
          </section>
        )}

        {dir && changes.length > 0 && (
          <footer className="side-panel-section">
            <label className="field">
              Commit message
              <textarea
                className="commit-message"
                value={message}
                onChange={(e) => {
                  setMessage(e.target.value);
                  setMessageEdited(true);
                }}
                rows={5}
              />
            </label>
            <div className="actions">
              <button onClick={() => save(false)} disabled={saving}>
                Write files
              </button>
              <span className="spacer" />
              <button
                className="primary"
                onClick={() => save(true)}
                disabled={saving || !status?.repository || !message.trim()}
                title={status?.repository ? undefined : "The folder isn't inside a Git repository"}
              >
                {saving ? "Saving…" : "Write & commit"}
              </button>
            </div>
          </footer>
        )}
        {result && (
          <p className={`side-panel-section ${result.error ? "error-text" : "muted"}`}>{result.text}</p>
        )}
      </aside>
    </div>
  );
}

function Diff({ text }: { text: string }) {
  return (
    <pre className="diff">
      {text.replace(/\n$/, "").split("\n").map((line, i) => (
        <div key={i} className={diffLineClass(line)}>
          {line || " "}
        </div>
      ))}
    </pre>
  );
}

function diffLineClass(line: string) {
  if (line.startsWith("+++") || line.startsWith("---")) return "diff-header";
  if (line.startsWith("@@")) return "diff-hunk";
  if (line.startsWith("+")) return "diff-add";
  if (line.startsWith("-")) return "diff-remove";
  return undefined;
}

function defaultMessage(database: string, changes: FileChange[]) {
  const verbs = { added: "Add", modified: "Update", removed: "Remove" } as const;
  const lines = changes.slice(0, 20).map((change) => `- ${verbs[change.kind]} ${change.path}`);
  if (changes.length > 20) lines.push(`- and ${changes.length - 20} more`);
  return `Update ${database} schema snapshot\n\n${lines.join("\n")}`;
}
