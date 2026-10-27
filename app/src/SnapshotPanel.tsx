import { ReactNode, useCallback, useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { Commit, FileChange, Repository, SnapshotRemote, SnapshotStatus, errorMessage, snapshots } from "./api";
import { ApplyDialog } from "./ApplyDialog";
import { GitGraph } from "./GitGraph";
import { Splitter } from "./Splitter";
import type { Messages } from "./i18n/en";
import { useT } from "./settings";

interface Props {
  sessionId: number;
  /** The database name, for the default commit message. */
  database: string;
  /** The snapshot folder: chosen, or inside Dolmen's clone of `remote` once it's ready. */
  dir: string | null;
  /** The repository Dolmen clones and pushes, when the snapshot lives in one. */
  remote: SnapshotRemote | null;
  /** Where Dolmen keeps its clone of `remote`. */
  clonePath: string | null;
  preparingRemote: boolean;
  onChooseDir: (dir: string) => void;
  onChooseRemote: (remote: SnapshotRemote) => void;
  /** Pulls new commits into the clone of `remote`; resolves to whether any arrived. */
  onSyncRemote: () => Promise<boolean>;
  onResetLocation: () => void;
  status: SnapshotStatus | null;
  statusError: string | null;
  checking: boolean;
  onRefresh: () => void;
  /** Called after a snapshot was applied to the database. */
  onDatabaseChanged: () => void;
  onClose: () => void;
  /** The panel's width in px, which the user can drag. */
  width: number;
  onResize: (width: number) => void;
  onResetWidth: () => void;
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
  remote,
  clonePath,
  preparingRemote,
  onChooseDir,
  onChooseRemote,
  onSyncRemote,
  onResetLocation,
  status,
  statusError,
  checking,
  onRefresh,
  onDatabaseChanged,
  onClose,
  width,
  onResize,
  onResetWidth,
}: Props) {
  const t = useT();
  const [tab, setTab] = useState<Tab>("changes");
  const [applyTarget, setApplyTarget] = useState<{ rev: string | null; label: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ text: string; error: boolean } | null>(null);
  const [historyVersion, setHistoryVersion] = useState(0);
  const repository = status?.repository ?? null;

  async function chooseDir() {
    const picked = await open({ directory: true, title: t.snapshot.chooseFolderTitle });
    if (typeof picked === "string") {
      setResult(null);
      onChooseDir(picked);
    }
  }

  /** Compares again, pulling the remote's new commits first when there is one. */
  async function refresh() {
    setHistoryVersion((v) => v + 1);
    if (remote) {
      try {
        await onSyncRemote();
      } catch {
        return; // The error shows in the panel.
      }
    }
    onRefresh();
  }

  /** Runs a Git action, then reloads the status and history. Resolves to whether it worked. */
  async function act(action: () => Promise<string>) {
    setBusy(true);
    setResult(null);
    try {
      setResult({ text: await action(), error: false });
      onRefresh();
      setHistoryVersion((v) => v + 1);
      return true;
    } catch (error) {
      setResult({ text: errorMessage(error, t), error: true });
      return false;
    } finally {
      setBusy(false);
    }
  }

  const push = () =>
    act(async () => {
      await snapshots.push(dir!);
      return t.snapshot.pushed;
    });

  const createBranch = (name: string) =>
    act(async () => {
      await snapshots.createBranch(dir!, name);
      return t.snapshot.branches.created(name);
    });

  // Like checking out code: the files change, then the database can follow.
  const switchBranch = (name: string) =>
    act(async () => {
      await snapshots.switchBranch(dir!, name);
      setApplyTarget({ rev: null, label: t.snapshot.branches.label(name) });
      return t.snapshot.branches.switched(name);
    });

  // Database changes not in the snapshot, which matching another branch would undo.
  const unsaved = status?.changes.filter((change) => change.path !== MODEL_FILE).length ?? 0;

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
      <aside className="side-panel" role="dialog" aria-label={t.snapshot.title} style={{ width: `min(${width}px, 100%)` }}>
        <Splitter
          orientation="vertical"
          size={width}
          bounds={() => [400, window.innerWidth - 80]}
          grows={-1}
          onResize={onResize}
          onReset={onResetWidth}
          label={t.workspace.resizePanel}
        />
        <header className="side-panel-header">
          <strong>{t.snapshot.title}</strong>
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
                  {t.snapshot.tabs[name]}
                </button>
              ))}
            </div>
          )}
          <span className="spacer" />
          <button
            onClick={refresh}
            disabled={!dir || checking || preparingRemote}
            title={remote ? t.snapshot.pullAndCompare : t.snapshot.compareAgain}
          >
            ↻
          </button>
          <button onClick={onClose} title={t.common.close}>
            ✕
          </button>
        </header>

        <section className="side-panel-section">
          {remote ? (
            <>
              <div className="snapshot-folder">
                <code title={remote.url}>{remote.url}</code>
                <span className="muted">{remote.path}</span>
                <button onClick={onResetLocation} disabled={busy}>
                  {t.common.change}
                </button>
              </div>
              <p className="muted">
                {preparingRemote ? t.snapshot.gettingRepository : clonePath && t.snapshot.cloneLocation(clonePath)}
              </p>
              <SshHint remote={remote} onChooseRemote={onChooseRemote} />
            </>
          ) : dir ? (
            <div className="snapshot-folder">
              <code title={dir}>{dir}</code>
              <button onClick={onResetLocation} disabled={busy}>
                {t.common.change}
              </button>
            </div>
          ) : (
            <LocationChooser
              onChooseDir={chooseDir}
              onChooseRemote={(chosen) => {
                setResult(null);
                onChooseRemote(chosen);
              }}
            />
          )}
          {statusError && !dir && <p className="error-text">{statusError}</p>}
          {dir && status && !repository && <p className="muted">{t.snapshot.notInRepository}</p>}
          {repository && (
            <>
              <p className="muted">Git: {repository.root}</p>
              <BranchBar
                repository={repository}
                unsaved={unsaved}
                busy={busy}
                onCreate={createBranch}
                onSwitch={switchBranch}
              >
                {toPush !== 0 && (
                  <button onClick={push} disabled={busy}>
                    {toPush === "unpublished" ? t.snapshot.publishBranch : t.snapshot.push(toPush)}
                  </button>
                )}
              </BranchBar>
            </>
          )}
          {dir && status && !status.initialized && <p className="muted">{t.snapshot.firstWrite}</p>}
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
            remote={remote !== null}
            onSyncRemote={onSyncRemote}
            onApplyFiles={() =>
              setApplyTarget({ rev: null, label: remote ? t.snapshot.latestSnapshot : t.snapshot.snapshotFiles })
            }
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
              setResult({ text: t.snapshot.updatedDatabase(applyTarget.label), error: false });
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

const MODEL_FILE = "dolmen-model.json";

/**
 * Shows the checked-out branch and switches or creates branches. Switching
 * while the database has changes the snapshot doesn't have asks first, since
 * matching the other branch would undo them.
 */
function BranchBar({
  repository,
  unsaved,
  busy,
  onCreate,
  onSwitch,
  children,
}: {
  repository: Repository;
  unsaved: number;
  busy: boolean;
  onCreate: (name: string) => Promise<boolean>;
  onSwitch: (name: string) => Promise<boolean>;
  children: ReactNode;
}) {
  const t = useT();
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [confirming, setConfirming] = useState<string | null>(null);

  // A new repository's branch has no commits, so Git doesn't list it yet.
  const branches =
    repository.branch && !repository.branches.includes(repository.branch)
      ? [repository.branch, ...repository.branches]
      : repository.branches;

  function pick(branch: string) {
    setCreating(false);
    if (unsaved > 0) setConfirming(branch);
    else onSwitch(branch);
  }

  return (
    <>
      <div className="repo-line">
        <label className="branch-picker">
          {t.snapshot.branches.branch}
          <select value={repository.branch ?? ""} onChange={(e) => pick(e.target.value)} disabled={busy}>
            {!repository.branch && (
              <option value="" disabled>
                {t.snapshot.branches.detached}
              </option>
            )}
            {branches.map((branch) => (
              <option key={branch} value={branch}>
                {branch}
              </option>
            ))}
          </select>
        </label>
        <span className="muted">
          {repository.upstream && `→ ${repository.upstream}`}
          {repository.behind > 0 && ` · ${t.snapshot.behind(repository.behind)}`}
        </span>
        <span className="spacer" />
        <button
          onClick={() => {
            setConfirming(null);
            setCreating(!creating);
          }}
          disabled={busy}
        >
          {t.snapshot.branches.newBranch}
        </button>
        {children}
      </div>
      {creating && (
        <form
          className="repo-line"
          onSubmit={async (e) => {
            e.preventDefault();
            if (await onCreate(name.trim())) {
              setCreating(false);
              setName("");
            }
          }}
        >
          <input
            autoFocus
            value={name}
            placeholder={t.snapshot.branches.namePlaceholder}
            onChange={(e) => setName(e.target.value)}
            aria-label={t.snapshot.branches.newBranch}
          />
          <button type="button" onClick={() => setCreating(false)}>
            {t.common.cancel}
          </button>
          <button type="submit" className="primary" disabled={busy || !name.trim()}>
            {t.snapshot.branches.create}
          </button>
        </form>
      )}
      {confirming && (
        <div className="error-box">
          <p>{t.snapshot.branches.unsaved(unsaved, confirming)}</p>
          <div className="actions">
            <span className="spacer" />
            <button onClick={() => setConfirming(null)}>{t.common.cancel}</button>
            <button
              className="primary danger-fill"
              onClick={() => {
                onSwitch(confirming);
                setConfirming(null);
              }}
              disabled={busy}
            >
              {t.snapshot.branches.switchAnyway}
            </button>
          </div>
        </div>
      )}
    </>
  );
}

/**
 * Pushing over HTTPS uses whichever account the credential helper saved, and
 * SSH to the plain host uses the default key; either fails when the Mac has
 * several accounts. Offers the repository's SSH addresses, one per matching
 * alias in ~/.ssh/config, which pick the account by key.
 */
function SshHint({
  remote,
  onChooseRemote,
}: {
  remote: SnapshotRemote;
  onChooseRemote: (remote: SnapshotRemote) => void;
}) {
  const t = useT();
  const [alternatives, setAlternatives] = useState<string[]>([]);

  useEffect(() => {
    setAlternatives([]);
    snapshots
      .sshAlternatives(remote.url)
      .then(setAlternatives)
      .catch(() => {});
  }, [remote.url]);

  if (alternatives.length === 0) return null;
  return (
    <div className="ssh-hint">
      <p className="muted">{/^https?:\/\//.test(remote.url) ? t.snapshot.httpsHint : t.snapshot.sshHint}</p>
      {alternatives.map((url) => (
        <button key={url} onClick={() => onChooseRemote({ url, path: remote.path })}>
          {t.snapshot.useAddress(url)}
        </button>
      ))}
    </div>
  );
}

function LocationChooser({
  onChooseDir,
  onChooseRemote,
}: {
  onChooseDir: () => void;
  onChooseRemote: (remote: SnapshotRemote) => void;
}) {
  const t = useT();
  const [url, setUrl] = useState("");
  const [path, setPath] = useState("db/schema");

  return (
    <div className="location-chooser">
      <p>{t.snapshot.location.intro}</p>
      <form
        className="location-choice"
        onSubmit={(e) => {
          e.preventDefault();
          onChooseRemote({ url: url.trim(), path: path.trim() });
        }}
      >
        <strong>{t.snapshot.location.repository}</strong>
        <p className="muted">{t.snapshot.location.repositoryHelp}</p>
        <div className="field-row">
          <label className="field grow">
            {t.snapshot.location.repositoryUrl}
            <input
              value={url}
              placeholder="git@github.com:you/schema.git"
              onChange={(e) => setUrl(e.target.value)}
              required
            />
          </label>
          <label className="field">
            {t.snapshot.location.folderInRepository}
            <input value={path} onChange={(e) => setPath(e.target.value)} required />
          </label>
        </div>
        <div className="actions">
          <span className="spacer" />
          <button type="submit" className="primary" disabled={!url.trim() || !path.trim()}>
            {t.snapshot.location.useRepository}
          </button>
        </div>
      </form>
      <div className="location-choice">
        <strong>{t.snapshot.location.folder}</strong>
        <p className="muted">{t.snapshot.location.folderHelp}</p>
        <div className="actions">
          <span className="spacer" />
          <button onClick={onChooseDir}>{t.snapshot.location.chooseFolder}</button>
        </div>
      </div>
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
  /** Whether the snapshot lives in a repository Dolmen manages, which it pushes on every commit. */
  remote: boolean;
  onSyncRemote: () => Promise<boolean>;
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
  remote,
  onSyncRemote,
  onApplyFiles,
}: ChangesTabProps) {
  const t = useT();
  const changes = status?.changes ?? [];
  const repository = status?.repository ?? null;
  const [message, setMessage] = useState("");
  const [messageEdited, setMessageEdited] = useState(false);

  // Follow the changes with the default message until the user edits it.
  useEffect(() => {
    if (!messageEdited) setMessage(defaultMessage(database, changes, t));
  }, [status, database, messageEdited, t]);

  const save = (commit: boolean, push: boolean) =>
    act(async () => {
      // Pull first. If someone else's commits arrive, the reviewed changes are
      // out of date, and committing now could undo theirs.
      if (remote && (await onSyncRemote())) {
        throw new Error(t.snapshot.changes.newCommitsArrived);
      }
      const saved = await snapshots.save(sessionId, dir, commit ? message.trim() : null, push);
      setMessageEdited(false);
      if (!saved.commit) return t.snapshot.changes.wrote(changes.length);
      return t.snapshot.changes.committed(saved.commit.slice(0, 7), changes.length, saved.pushed);
    });

  return (
    <>
      <section className="side-panel-section grow">
        {statusError && <p className="error-text">{statusError}</p>}
        {!status && checking && <p className="muted">{t.snapshot.changes.comparing}</p>}
        {status && changes.length === 0 && <p className="muted">{t.snapshot.changes.matches}</p>}
        {changes.length > 0 && (
          <>
            <p className="muted">{t.snapshot.changes.count(changes.length)}</p>
            <ChangeList changes={changes} />
          </>
        )}
      </section>

      {changes.length > 0 && (
        <footer className="side-panel-section">
          <label className="field">
            {t.snapshot.changes.commitMessage}
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
            {!remote && (
              <button onClick={() => save(false, false)} disabled={busy}>
                {t.snapshot.changes.writeFiles}
              </button>
            )}
            {status?.initialized && (
              <button onClick={onApplyFiles} disabled={busy} title={t.snapshot.changes.updateDatabaseTitle}>
                {t.snapshot.changes.updateDatabase}
              </button>
            )}
            <span className="spacer" />
            {!remote && (
              <button
                onClick={() => save(true, false)}
                disabled={busy || !repository || !message.trim()}
                title={repository ? undefined : t.snapshot.changes.notInRepositoryTitle}
              >
                {t.snapshot.changes.commit}
              </button>
            )}
            {repository && repository.remotes.length > 0 && (
              <button className="primary" onClick={() => save(true, true)} disabled={busy || !message.trim()}>
                {busy ? t.common.saving : t.snapshot.changes.commitAndPush}
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
  const t = useT();
  const [commits, setCommits] = useState<Commit[] | null>(null);
  const [error, setError] = useState<unknown>(null);
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
      .catch(setError);
  }, [dir, version]);

  const compare = useCallback(
    (commit: Commit) => {
      setSelected(commit);
      setComparison(null);
      setComparing(true);
      snapshots
        .compare(sessionId, dir, commit.hash)
        .then(setComparison)
        .catch(setError)
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
      {error !== null && <p className="error-text">{errorMessage(error, t)}</p>}
      {commits && commits.length === 0 && <p className="muted">{t.snapshot.history.noCommits}</p>}
      {commits && commits.length > 0 && (
        <GitGraph commits={commits} selected={selected?.hash ?? null} onSelect={compare} />
      )}
      {selected && (
        <div className="comparison">
          <p>
            <strong>{t.snapshot.history.comparingTo(label)}</strong>{" "}
            <span className="muted">{t.snapshot.history.comparisonHelp}</span>
          </p>
          {comparing && <p className="muted">{t.snapshot.history.comparing}</p>}
          {comparison && comparison.length === 0 && (
            <p className="muted">{t.snapshot.history.alreadyMatches}</p>
          )}
          {comparison && comparison.length > 0 && (
            <>
              <ChangeList changes={comparison} />
              <div className="actions">
                <button className="primary" onClick={() => onApply(selected.hash, label)}>
                  {t.snapshot.history.updateDatabase(label)}
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
  const t = useT();
  // Only the model file comes without a diff.
  if (!text) return <p className="muted diff-note">{t.snapshot.changes.modelNote}</p>;
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

function defaultMessage(database: string, changes: FileChange[], t: Messages) {
  const verbs = t.snapshot.changes.messageLine;
  const lines = changes.slice(0, 20).map((change) => `- ${verbs[change.kind]} ${change.path}`);
  if (changes.length > 20) lines.push(`- ${t.snapshot.changes.andMore(changes.length - 20)}`);
  return t.snapshot.changes.defaultMessage(database, lines);
}
