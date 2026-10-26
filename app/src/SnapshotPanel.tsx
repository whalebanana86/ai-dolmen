import { useCallback, useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { Commit, FileChange, SnapshotStatus, errorMessage, snapshots } from "./api";
import { ApplyDialog } from "./ApplyDialog";
import { GitGraph } from "./GitGraph";

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
  /** Called after a snapshot was applied to the database. */
  onDatabaseChanged: () => void;
  onClose: () => void;
}

type Tab = "changes" | "history";

/**
 * Compares the database with its snapshot folder, writes, commits and pushes
 * the snapshot, and shows the folder's Git history.
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
  onDatabaseChanged,
  onClose,
}: Props) {
  const [tab, setTab] = useState<Tab>("changes");
  const [applyTarget, setApplyTarget] = useState<{ rev: string | null; label: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ text: string; error: boolean } | null>(null);
  const [historyVersion, setHistoryVersion] = useState(0);
  const repository = status?.repository ?? null;

  async function chooseDir() {
    const picked = await open({ directory: true, title: "Choose the schema snapshot folder" });
    if (typeof picked === "string") {
      setResult(null);
      onChooseDir(picked);
    }
  }

  /** Runs a Git action, then reloads the status and history. */
  async function act(action: () => Promise<string>) {
    setBusy(true);
    setResult(null);
    try {
      setResult({ text: await action(), error: false });
      onRefresh();
      setHistoryVersion((v) => v + 1);
    } catch (error) {
      setResult({ text: errorMessage(error), error: true });
    } finally {
      setBusy(false);
    }
  }

  const push = () =>
    act(async () => {
      await snapshots.push(dir!);
      return "Pushed";
    });

  // Commits waiting to be pushed, or a branch that was never pushed.
  const toPush = !repository
    ? 0
    : repository.upstream
      ? repository.ahead
      : repository.remotes.length > 0
        ? "unpublished"
        : 0;

  return (
    <div className="panel-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <aside className="side-panel" role="dialog" aria-label="Schema snapshot">
        <header className="side-panel-header">
          <strong>Schema snapshot</strong>
          {dir && (
            <div className="tabs" role="tablist">
              {(["changes", "history"] as const).map((name) => (
                <button
                  key={name}
                  role="tab"
                  aria-selected={tab === name}
                  className={tab === name ? "selected" : undefined}
                  onClick={() => setTab(name)}
                >
                  {name === "changes" ? "Changes" : "History"}
                </button>
              ))}
            </div>
          )}
          <span className="spacer" />
          <button
            onClick={() => {
              onRefresh();
              setHistoryVersion((v) => v + 1);
            }}
            disabled={!dir || checking}
            title="Compare again"
          >
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
          {dir && status && !repository && (
            <p className="muted">Not inside a Git repository: the files can be written but not committed.</p>
          )}
          {repository && (
            <div className="repo-line">
              <span className="muted">
                Git: {repository.root}
                {repository.branch && ` · ${repository.branch}`}
                {repository.upstream && ` → ${repository.upstream}`}
                {repository.behind > 0 && ` · ${repository.behind} behind`}
              </span>
              <span className="spacer" />
              {toPush !== 0 && (
                <button onClick={push} disabled={busy}>
                  {toPush === "unpublished" ? "Publish branch" : `Push ${toPush}`}
                </button>
              )}
            </div>
          )}
          {dir && status && !status.initialized && (
            <p className="muted">The first write sets this folder up as a snapshot folder.</p>
          )}
        </section>

        {dir && tab === "changes" && (
          <ChangesTab
            sessionId={sessionId}
            database={database}
            dir={dir}
            status={status}
            statusError={statusError}
            checking={checking}
            busy={busy}
            act={act}
            onApplyFiles={() => setApplyTarget({ rev: null, label: "the snapshot files" })}
          />
        )}
        {dir && tab === "history" && (
          <HistoryTab
            sessionId={sessionId}
            dir={dir}
            version={historyVersion}
            onApply={(rev, label) => setApplyTarget({ rev, label })}
          />
        )}
        {dir && applyTarget && (
          <ApplyDialog
            sessionId={sessionId}
            dir={dir}
            rev={applyTarget.rev}
            label={applyTarget.label}
            onClose={() => setApplyTarget(null)}
            onApplied={() => {
              setResult({ text: `Updated the database to match ${applyTarget.label}`, error: false });
              onRefresh();
              onDatabaseChanged();
              setHistoryVersion((v) => v + 1);
            }}
          />
        )}

        {result && (
          <p className={`side-panel-section ${result.error ? "error-text" : "muted"}`}>{result.text}</p>
        )}
      </aside>
    </div>
  );
}

interface ChangesTabProps {
  sessionId: number;
  database: string;
  dir: string;
  status: SnapshotStatus | null;
  statusError: string | null;
  checking: boolean;
  busy: boolean;
  act: (action: () => Promise<string>) => void;
  /** Opens the plan that makes the database match the folder's files. */
  onApplyFiles: () => void;
}

function ChangesTab({
  sessionId,
  database,
  dir,
  status,
  statusError,
  checking,
  busy,
  act,
  onApplyFiles,
}: ChangesTabProps) {
  const changes = status?.changes ?? [];
  const repository = status?.repository ?? null;
  const [message, setMessage] = useState("");
  const [messageEdited, setMessageEdited] = useState(false);

  // Follow the changes with the default message until the user edits it.
  useEffect(() => {
    if (!messageEdited) setMessage(defaultMessage(database, changes));
  }, [status, database, messageEdited]);

  const save = (commit: boolean, push: boolean) =>
    act(async () => {
      const saved = await snapshots.save(sessionId, dir, commit ? message.trim() : null, push);
      setMessageEdited(false);
      const files = `${changes.length} ${changes.length === 1 ? "file" : "files"}`;
      if (!saved.commit) return `Wrote ${files}`;
      return `${saved.pushed ? "Committed and pushed" : "Committed"} ${saved.commit.slice(0, 7)} (${files})`;
    });

  return (
    <>
      <section className="side-panel-section grow">
        {statusError && <p className="error-text">{statusError}</p>}
        {!status && checking && <p className="muted">Comparing with the database…</p>}
        {status && changes.length === 0 && <p className="muted">The snapshot matches the database.</p>}
        {changes.length > 0 && (
          <>
            <p className="muted">
              {changes.length} {changes.length === 1 ? "change" : "changes"} since the last snapshot
            </p>
            <ChangeList changes={changes} />
          </>
        )}
      </section>

      {changes.length > 0 && (
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
            <button onClick={() => save(false, false)} disabled={busy}>
              Write files
            </button>
            {status?.initialized && (
              <button
                onClick={onApplyFiles}
                disabled={busy}
                title="Change the database to match the files, e.g. after checking out an older commit"
              >
                Update database…
              </button>
            )}
            <span className="spacer" />
            <button
              onClick={() => save(true, false)}
              disabled={busy || !repository || !message.trim()}
              title={repository ? undefined : "The folder isn't inside a Git repository"}
            >
              Commit
            </button>
            {repository && repository.remotes.length > 0 && (
              <button className="primary" onClick={() => save(true, true)} disabled={busy || !message.trim()}>
                {busy ? "Saving…" : "Commit & push"}
              </button>
            )}
          </div>
        </footer>
      )}
    </>
  );
}

interface HistoryTabProps {
  sessionId: number;
  dir: string;
  version: number;
  onApply: (rev: string, label: string) => void;
}

function HistoryTab({ sessionId, dir, version, onApply }: HistoryTabProps) {
  const [commits, setCommits] = useState<Commit[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Commit | null>(null);
  const [comparison, setComparison] = useState<FileChange[] | null>(null);
  const [comparing, setComparing] = useState(false);

  useEffect(() => {
    snapshots
      .history(dir)
      .then((history) => {
        setCommits(history);
        setError(null);
      })
      .catch((err) => setError(errorMessage(err)));
  }, [dir, version]);

  const compare = useCallback(
    (commit: Commit) => {
      setSelected(commit);
      setComparison(null);
      setComparing(true);
      snapshots
        .compare(sessionId, dir, commit.hash)
        .then(setComparison)
        .catch((err) => setError(errorMessage(err)))
        .finally(() => setComparing(false));
    },
    [sessionId, dir],
  );

  // Compare again after the database or history changed.
  useEffect(() => {
    if (selected) compare(selected);
    // Only on reloads, not when a commit is picked.
  }, [version]);

  const label = selected
    ? (selected.refs.find((ref) => ref.startsWith("tag: "))?.replace(/^tag: /, "") ?? selected.hash.slice(0, 7))
    : "";

  return (
    <section className="side-panel-section grow history">
      {error && <p className="error-text">{error}</p>}
      {commits && commits.length === 0 && <p className="muted">No commits yet.</p>}
      {commits && commits.length > 0 && (
        <GitGraph commits={commits} selected={selected?.hash ?? null} onSelect={compare} />
      )}
      {selected && (
        <div className="comparison">
          <p>
            <strong>Database → {label}</strong>{" "}
            <span className="muted">what would change to make the database match this commit</span>
          </p>
          {comparing && <p className="muted">Comparing…</p>}
          {comparison && comparison.length === 0 && (
            <p className="muted">The database already matches this commit's snapshot.</p>
          )}
          {comparison && comparison.length > 0 && (
            <>
              <ChangeList changes={comparison} />
              <div className="actions">
                <button className="primary" onClick={() => onApply(selected.hash, label)}>
                  Update database to match {label}…
                </button>
              </div>
            </>
          )}
        </div>
      )}
    </section>
  );
}

const KIND_LETTERS = { added: "A", modified: "M", removed: "D" } as const;

function ChangeList({ changes }: { changes: FileChange[] }) {
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const toggle = (path: string) =>
    setExpanded((current) => {
      const next = new Set(current);
      if (!next.delete(path)) next.add(path);
      return next;
    });

  return (
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
  );
}

function Diff({ text }: { text: string }) {
  if (!text) {
    return (
      <p className="muted diff-note">
        The schema model the DDL was written from. It lets Dolmen apply this snapshot to a database later.
      </p>
    );
  }
  return (
    <pre className="diff">
      {text
        .replace(/\n$/, "")
        .split("\n")
        .map((line, i) => (
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
