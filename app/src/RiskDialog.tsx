import { Assessment, Environment } from "./api";
import { EnvironmentBadge } from "./ConnectionForm";
import type { Messages } from "./i18n/en";
import { useT } from "./settings";

export interface RiskyStatement {
  statement: string;
  assessment: Assessment;
}

interface Props {
  risky: RiskyStatement[];
  /** The database they would run on: the connection's name and address. */
  target: { label: string; address: string; environment: Environment | null };
  onRun: () => void;
  onCancel: () => void;
}

/**
 * Asks before running statements that change every row or drop something,
 * saying what each would destroy and about how many rows.
 */
export function RiskDialog({ risky, target, onRun, onCancel }: Props) {
  const t = useT();
  const production = target.environment === "production";

  return (
    <div className="dialog-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <div
        className="dialog risk-dialog"
        role="alertdialog"
        aria-label={t.risk.title}
        onKeyDown={(e) => e.key === "Escape" && onCancel()}
      >
        <header className="side-panel-header">
          <strong>{t.risk.title}</strong>
          <span className="spacer" />
          <button onClick={onCancel} title={t.common.close}>
            ✕
          </button>
        </header>
        <div className="dialog-body">
          <p className="risk-target">
            {target.environment && <EnvironmentBadge environment={target.environment} />}
            <span>{t.risk.target(target.label, target.address)}</span>
          </p>
          <ul className="risk-list">
            {risky.map(({ statement, assessment }, i) => (
              <li key={i}>
                <p>
                  <strong>{riskText(assessment, t)}</strong>
                </p>
                <code className="risk-sql" title={statement}>
                  {statement.replace(/\s+/g, " ").trim()}
                </code>
              </li>
            ))}
          </ul>
        </div>
        <footer className="dialog-footer">
          <div className="actions">
            <span className="spacer" />
            {/* Cancelling is the default: Enter doesn't run them. */}
            <button onClick={onCancel} autoFocus>
              {t.common.cancel}
            </button>
            <button className="primary danger-fill" onClick={onRun}>
              {production ? t.risk.runOnProduction : t.risk.run}
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
}

function riskText({ risk, estimate }: Assessment, t: Messages) {
  const rows = estimate ? t.risk.rows(estimate.rows, estimate.atLeast) : null;
  switch (risk.kind) {
    case "everyRow":
      return risk.verb === "DELETE" ? t.risk.deleteAll(risk.table, rows) : t.risk.updateAll(risk.table, rows);
    case "truncate":
      return t.risk.truncate(risk.table, rows);
    case "drop":
      return t.risk.drop(risk.object, rows);
    case "dropColumn":
      return t.risk.dropColumn(risk.table, risk.column, rows);
  }
}
