import { useEffect, useState } from "react";
import { ApplyPreview, Blocker, Rename, errorMessage, snapshots } from "./api";
import type { Messages } from "./i18n/en";
import { useT } from "./settings";

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
  const t = useT();
  const [renames, setRenames] = useState<Rename[]>([]);
  const [preview, setPreview] = useState<ApplyPreview | null>(null);
  const [loading, setLoading] = useState(true);
  const [allowDestructive, setAllowDestructive] = useState(false);
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<unknown>(null);

  useEffect(() => {
    setLoading(true);
    setError(null);
    snapshots
      .plan(sessionId, dir, rev, renames)
      .then(setPreview)
      .catch((err) => {
        setPreview(null);
        setError(err);
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
      setError(err);
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
      <div className="dialog" role="dialog" aria-label={t.apply.title(label)}>
        <header className="side-panel-header">
          <strong>{t.apply.title(label)}</strong>
          <span className="spacer" />
          <button onClick={onClose} disabled={applying} title={t.common.close}>
            ✕
          </button>
        </header>

        <div className="dialog-body">
          {preview && !preview.local && (
            <p className="error-box">{t.apply.notLocal}</p>
          )}
          {loading && <p className="muted">{t.apply.planning}</p>}
          {plan && plan.blockers.length > 0 && (
            <div className="error-box">
              <p>
                <strong>{t.apply.blockersTitle}</strong>
              </p>
              <ul>
                {plan.blockers.map((blocker) => (
                  <li key={JSON.stringify(blocker)}>{blockerText(blocker, t)}</li>
                ))}
              </ul>
            </div>
          )}

          {renameChoices.length > 0 && (
            <div className="rename-choices">
              <p className="muted">{t.apply.renamesIntro}</p>
              {renameChoices.map((rename) => (
                <label key={JSON.stringify(rename)} className="checkbox">
                  <input
                    type="checkbox"
                    checked={renames.some((r) => sameRename(r, rename))}
                    onChange={() => toggleRename(rename)}
                    disabled={loading || applying}
                  />
                  {renameLabel(rename, t)}
                </label>
              ))}
            </div>
          )}

          {plan && plan.steps.length === 0 && plan.blockers.length === 0 && (
            <p className="muted">{t.apply.alreadyMatches(label)}</p>
          )}
          {plan && plan.steps.length > 0 && (
            <>
              <p className="muted">{t.apply.steps(plan.steps.length)}</p>
              <ol className="plan-steps">
                {plan.steps.map((step, i) => (
                  <li key={i} className={step.destructive ? "destructive" : undefined}>
                    {step.destructive && <span className="step-warning">{t.apply.losesData}</span>}
                    <pre>{step.sql}</pre>
                  </li>
                ))}
              </ol>
            </>
          )}
          {error !== null && <p className="error-text">{errorMessage(error, t)}</p>}
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
              {t.apply.allowDestructive(destructive)}
            </label>
          )}
          <div className="actions">
            <span className="spacer" />
            <button onClick={onClose} disabled={applying}>
              {t.common.cancel}
            </button>
            <button className={destructive > 0 ? "primary danger-fill" : "primary"} onClick={apply} disabled={!canApply}>
              {applying ? t.apply.applying : t.apply.apply}
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

function renameLabel(rename: Rename, t: Messages) {
  return rename.kind === "table"
    ? t.apply.renameTable(`${rename.schema}.${rename.from}`, rename.to)
    : t.apply.renameColumn(`${rename.schema}.${rename.table}.${rename.from}`, rename.to);
}

export function blockerText(blocker: Blocker, t: Messages) {
  switch (blocker.kind) {
    case "enumValuesRemoved":
      return t.apply.blockers.enumValuesRemoved(blocker.name);
    case "tableKindChanged":
      return t.apply.blockers.tableKindChanged(blocker.name);
    case "generatedColumnChanged":
      return t.apply.blockers.generatedColumnChanged(blocker.name);
    case "renameNoLongerApplies":
      return t.apply.blockers.renameNoLongerApplies(blocker.from, blocker.to);
    case "invalidDefinition":
      return t.apply.blockers.invalidDefinition(blocker.name);
  }
}
