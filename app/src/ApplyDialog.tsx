import { useEffect, useState } from "react";
import { ApplyPreview, Rename, errorMessage, snapshots } from "./api";

interface Props {
  sessionId: number;
  dir: string;
  /** The commit or tag to match, or null for the folder's files. */
  rev: string | null;
  /** How to name the target, e.g. "v1.0" or "the snapshot files". */
  label: string;
  onClose: () => void;
  onApplied: () => void;
}

/**
 * Shows the plan that makes the database match a snapshot, and applies it in
 * one transaction once reviewed. Steps that drop data stay blocked until the
 * user allows them, and only local databases can be changed.
 */
export function ApplyDialog({ sessionId, dir, rev, label, onClose, onApplied }: Props) {
  const [renames, setRenames] = useState<Rename[]>([]);
  const [preview, setPreview] = useState<ApplyPreview | null>(null);
  const [loading, setLoading] = useState(true);
  const [allowDestructive, setAllowDestructive] = useState(false);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setLoading(true);
    setError(null);
    snapshots
      .plan(sessionId, dir, rev, renames)
      .then(setPreview)
      .catch((err) => {
        setPreview(null);
        setError(errorMessage(err));
      })
      .finally(() => setLoading(false));
  }, [sessionId, dir, rev, renames]);

  const plan = preview?.plan;
  const destructive = plan?.steps.filter((step) => step.destructive).length ?? 0;
  const canApply =
    !!plan &&
    !!preview?.local &&
    !loading &&
    !applying &&
    plan.steps.length > 0 &&
    plan.blockers.length === 0 &&
    (destructive === 0 || allowDestructive);

  function toggleRename(rename: Rename) {
    setAllowDestructive(false);
    setRenames((current) =>
      current.some((r) => sameRename(r, rename))
        ? current.filter((r) => !sameRename(r, rename))
        : [...current, rename],
    );
  }

  async function apply() {
    if (!plan) return;
    setApplying(true);
    setError(null);
    try {
      await snapshots.apply(
        sessionId,
        dir,
        rev,
        renames,
        plan.steps.map((step) => step.sql),
        allowDestructive,
      );
      onApplied();
      onClose();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setApplying(false);
    }
  }

  // Taken renames no longer show up as candidates, so list both.
  const renameChoices = [
    ...renames,
    ...(plan?.renameCandidates ?? []).filter((c) => !renames.some((r) => sameRename(r, c))),
  ];

  return (
    <div className="dialog-backdrop" onMouseDown={(e) => e.target === e.currentTarget && !applying && onClose()}>
      <div className="dialog" role="dialog" aria-label={`Update the database to match ${label}`}>
        <header className="side-panel-header">
          <strong>Update the database to match {label}</strong>
          <span className="spacer" />
          <button onClick={onClose} disabled={applying} title="Close">
            ✕
          </button>
        </header>

        <div className="dialog-body">
          {preview && !preview.local && (
            <p className="error-box">
              Only local databases (localhost, a loopback address or a Unix socket) can be changed from a
              snapshot. This connection's server is elsewhere, so the plan is shown for review only.
            </p>
          )}
          {loading && <p className="muted">Planning…</p>}
          {plan && plan.blockers.length > 0 && (
            <div className="error-box">
              <p>
                <strong>Dolmen can't make these changes, so the plan can't be applied:</strong>
              </p>
              <ul>
                {plan.blockers.map((blocker) => (
                  <li key={blocker}>{blocker}</li>
                ))}
              </ul>
            </div>
          )}

          {renameChoices.length > 0 && (
            <div className="rename-choices">
              <p className="muted">These drops look like renames. Renaming keeps the data:</p>
              {renameChoices.map((rename) => (
                <label key={JSON.stringify(rename)} className="checkbox">
                  <input
                    type="checkbox"
                    checked={renames.some((r) => sameRename(r, rename))}
                    onChange={() => toggleRename(rename)}
                    disabled={loading || applying}
                  />
                  Rename {renameLabel(rename)}
                </label>
              ))}
            </div>
          )}

          {plan && plan.steps.length === 0 && plan.blockers.length === 0 && (
            <p className="muted">The database already matches {label}.</p>
          )}
          {plan && plan.steps.length > 0 && (
            <>
              <p className="muted">
                {plan.steps.length} {plan.steps.length === 1 ? "step" : "steps"}, run in one transaction: if
                any step fails, nothing changes.
              </p>
              <ol className="plan-steps">
                {plan.steps.map((step, i) => (
                  <li key={i} className={step.destructive ? "destructive" : undefined}>
                    {step.destructive && <span className="step-warning">Loses data</span>}
                    <pre>{step.sql}</pre>
                  </li>
                ))}
              </ol>
            </>
          )}
          {error && <p className="error-text">{error}</p>}
        </div>

        <footer className="dialog-footer">
          {destructive > 0 && (
            <label className="checkbox destructive-consent">
              <input
                type="checkbox"
                checked={allowDestructive}
                onChange={(e) => setAllowDestructive(e.target.checked)}
                disabled={applying}
              />
              Allow {destructive} {destructive === 1 ? "step" : "steps"} that drop data or change column types.
              Dropped data can't be brought back by applying a newer snapshot.
            </label>
          )}
          <div className="actions">
            <span className="spacer" />
            <button onClick={onClose} disabled={applying}>
              Cancel
            </button>
            <button className={destructive > 0 ? "primary danger-fill" : "primary"} onClick={apply} disabled={!canApply}>
              {applying ? "Applying…" : "Apply"}
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
}

function sameRename(a: Rename, b: Rename) {
  return JSON.stringify(a) === JSON.stringify(b);
}

function renameLabel(rename: Rename) {
  return rename.kind === "table"
    ? `table ${rename.schema}.${rename.from} → ${rename.to}`
    : `column ${rename.schema}.${rename.table}.${rename.from} → ${rename.to}`;
}
