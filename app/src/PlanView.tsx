import { useMemo, useState } from "react";
import { ExplainOutput, PlanNode } from "./api";
import { StatementLine } from "./ResultView";
import type { Messages } from "./i18n/en";
import { useT } from "./settings";

interface Props {
  output: ExplainOutput;
  /** Measured with ANALYZE, rather than only the planner's estimates. */
  analyzed: boolean;
  statement: string;
  /** Running the statement changes data, so measuring it rolls back. */
  changesData: boolean;
  onMeasure: () => void;
  onReveal: () => void;
}

type Warning =
  | { kind: "seqScan"; removed: number }
  | { kind: "misestimate"; planned: number; actual: number }
  | { kind: "diskSort"; kb: number }
  | { kind: "hashBatches"; batches: number };

interface Row {
  id: string;
  depth: number;
  node: PlanNode;
  hasChildren: boolean;
  /** Time (ms) or cost spent in the node itself, not in the nodes below it. */
  self: number;
  /** `self` as a share of the whole plan's. */
  share: number;
  warnings: Warning[];
}

/** The conditions and keys worth reading next to a node, in this order. */
const DETAILS = [
  "Index Cond",
  "Recheck Cond",
  "Hash Cond",
  "Merge Cond",
  "Join Filter",
  "Filter",
  "Sort Key",
  "Group Key",
] as const;

/**
 * EXPLAIN's plan as a tree: what each step does, how many rows it was
 * expected to give and gave, and how much of the time (or cost) it took
 * itself, with notes on the usual causes of a slow plan.
 */
export function PlanView({ output, analyzed, statement, changesData, onMeasure, onReveal }: Props) {
  const t = useT();
  const [collapsed, setCollapsed] = useState(new Set<string>());
  const [copied, setCopied] = useState(false);
  const top = output[0];
  const rows = useMemo(() => planRows(top.Plan, analyzed), [top, analyzed]);
  const heaviest = rows.length >= 3 ? rows.reduce((a, b) => (b.self > a.self ? b : a)) : null;

  const visible = rows.filter((row) => ![...collapsed].some((id) => row.id.startsWith(`${id}.`)));

  function toggle(id: string) {
    setCollapsed((current) => {
      const next = new Set(current);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  }

  function copy() {
    navigator.clipboard
      .writeText(JSON.stringify(output, null, 2))
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      })
      .catch((error) => console.error("copying the plan failed", error));
  }

  return (
    <div className="plan">
      <div className="plan-header">
        <strong>{t.plan.title}</strong>
        <span className={analyzed ? "plan-mode measured" : "plan-mode"}>
          {analyzed ? t.plan.measured : t.plan.estimated}
        </span>
        {top["Planning Time"] !== undefined && (
          <span className="muted">{t.plan.planning(formatMs(top["Planning Time"]))}</span>
        )}
        {top["Execution Time"] !== undefined && (
          <span className="muted">{t.plan.execution(formatMs(top["Execution Time"]))}</span>
        )}
        <span className="spacer" />
        {!analyzed && (
          <button
            onClick={onMeasure}
            className={changesData ? "danger" : undefined}
            title={changesData ? t.plan.measureChangesTitle : t.plan.measureTitle}
          >
            {changesData ? t.plan.measureChanges : t.plan.measure}
          </button>
        )}
        <button onClick={copy}>{copied ? t.plan.copied : t.plan.copyJson}</button>
      </div>
      <StatementLine sql={statement} onReveal={onReveal} />

      <div className="plan-grid" role="tree" aria-label={t.plan.title}>
        <div className="plan-row plan-head" aria-hidden="true">
          <span>{t.plan.node}</span>
          <span className="number">{t.plan.rows}</span>
          <span className="number">{analyzed ? t.plan.time : t.plan.cost}</span>
          <span>{t.plan.share}</span>
        </div>
        {visible.map((row) => (
          <PlanRow
            key={row.id}
            row={row}
            analyzed={analyzed}
            heaviest={row === heaviest}
            collapsed={collapsed.has(row.id)}
            onToggle={() => toggle(row.id)}
          />
        ))}
      </div>
    </div>
  );
}

function PlanRow({
  row,
  analyzed,
  heaviest,
  collapsed,
  onToggle,
}: {
  row: Row;
  analyzed: boolean;
  heaviest: boolean;
  collapsed: boolean;
  onToggle: () => void;
}) {
  const t = useT();
  const { node } = row;
  const loops = num(node["Actual Loops"]);
  const neverRan = analyzed && loops === 0;
  const details = DETAILS.flatMap((key) => {
    const value = node[key];
    if (value === undefined) return [];
    return [`${key}: ${Array.isArray(value) ? value.join(", ") : String(value)}`];
  });
  const removed = num(node["Rows Removed by Filter"]);
  if (removed) details.push(t.plan.removed(Math.round(removed * (loops || 1))));

  return (
    <div
      className={neverRan ? "plan-row never-ran" : "plan-row"}
      role="treeitem"
      aria-level={row.depth + 1}
      aria-expanded={row.hasChildren ? !collapsed : undefined}
    >
      <div className="plan-node" style={{ paddingLeft: row.depth * 16 }}>
        <div className="plan-node-title">
          {row.hasChildren ? (
            <button
              className="plan-toggle"
              onClick={onToggle}
              aria-label={collapsed ? t.plan.expand : t.plan.collapse}
            >
              {collapsed ? "▸" : "▾"}
            </button>
          ) : (
            <span className="plan-toggle" />
          )}
          <strong>{nodeTitle(node)}</strong>
          {heaviest && <span className="plan-heaviest">{analyzed ? t.plan.slowest : t.plan.costliest}</span>}
        </div>
        {details.map((detail) => (
          <p key={detail} className="plan-detail">
            {detail}
          </p>
        ))}
        {row.warnings.map((warning) => (
          <p key={warning.kind} className="plan-warning">
            <span aria-hidden="true">⚠</span> {warningText(warning, t)}
          </p>
        ))}
      </div>
      <span className="number">
        {neverRan
          ? t.plan.neverRan
          : analyzed
            ? t.plan.actualOf(formatRows(num(node["Actual Rows"])), formatRows(num(node["Plan Rows"])))
            : t.plan.planned(formatRows(num(node["Plan Rows"])))}
        {loops > 1 && <span className="muted"> {t.plan.loops(loops)}</span>}
      </span>
      <span className="number">{analyzed ? `${formatMs(row.self)} ms` : row.self.toFixed(1)}</span>
      <span className="plan-share">
        <span className="plan-bar">
          <span style={{ width: `${Math.max(row.share * 100, row.self > 0 ? 1 : 0)}%` }} />
        </span>
        <span className="number">{Math.round(row.share * 100)}%</span>
      </span>
    </div>
  );
}

/** The plan's nodes in tree order, with the time or cost each took itself. */
function planRows(plan: PlanNode, analyzed: boolean): Row[] {
  // A node's figures include the nodes below it; Actual Total Time is per loop.
  const total = (node: PlanNode) =>
    analyzed ? num(node["Actual Total Time"]) * (num(node["Actual Loops"]) || 0) : num(node["Total Cost"]);
  const rows: Row[] = [];
  /** Visits a node and returns whether its estimate was off. */
  function visit(node: PlanNode, depth: number, id: string): boolean {
    const children = node.Plans ?? [];
    const self = Math.max(0, total(node) - children.reduce((sum, child) => sum + total(child), 0));
    const row: Row = { id, depth, node, hasChildren: children.length > 0, self, share: 0, warnings: [] };
    rows.push(row);
    const offBelow = children.map((child, i) => visit(child, depth + 1, `${id}.${i}`)).some(Boolean);
    const found = warnings(node, analyzed);
    const off = found.some((w) => w.kind === "misestimate");
    // A wrong estimate carries up to every node above it; say so only where it starts.
    row.warnings = offBelow ? found.filter((w) => w.kind !== "misestimate") : found;
    return off || offBelow;
  }
  visit(plan, 0, "0");
  const sum = rows.reduce((acc, row) => acc + row.self, 0);
  for (const row of rows) row.share = sum > 0 ? row.self / sum : 0;
  return rows;
}

function warnings(node: PlanNode, analyzed: boolean): Warning[] {
  const found: Warning[] = [];
  const loops = num(node["Actual Loops"]);
  if (analyzed && loops > 0) {
    const actual = num(node["Actual Rows"]);
    const planned = num(node["Plan Rows"]);
    const removed = num(node["Rows Removed by Filter"]) * loops;
    if (node["Node Type"] === "Seq Scan" && removed >= 1000 && removed > 10 * actual * loops) {
      found.push({ kind: "seqScan", removed: Math.round(removed) });
    }
    // Off by ten times or more, on enough rows to matter.
    const [low, high] = actual < planned ? [actual, planned] : [planned, actual];
    if (high >= 100 && high >= 10 * Math.max(low, 1)) {
      found.push({ kind: "misestimate", planned, actual });
    }
  }
  if (node["Sort Space Type"] === "Disk") found.push({ kind: "diskSort", kb: num(node["Sort Space Used"]) });
  const batches = num(node["Hash Batches"]);
  if (batches > 1) found.push({ kind: "hashBatches", batches });
  return found;
}

function warningText(warning: Warning, t: Messages) {
  switch (warning.kind) {
    case "seqScan":
      return t.plan.warnings.seqScan(warning.removed);
    case "misestimate":
      return t.plan.warnings.misestimate(warning.planned, warning.actual);
    case "diskSort":
      return t.plan.warnings.diskSort(warning.kb);
    case "hashBatches":
      return t.plan.warnings.hashBatches(warning.batches);
  }
}

/** E.g. "Hash Join (Left)", "Index Scan using orders_pkey on orders o", "CTE Scan on recent". */
function nodeTitle(node: PlanNode) {
  let title = String(node["Node Type"]);
  const joinType = node["Join Type"];
  if (joinType && joinType !== "Inner") title += ` (${joinType})`;
  const strategy = node.Strategy;
  if (node["Node Type"] === "Aggregate" && strategy && strategy !== "Plain") title += ` (${strategy})`;
  if (node["Index Name"]) title += ` using ${node["Index Name"]}`;
  const relation = node["Relation Name"];
  if (relation) {
    const schema = node.Schema ? `${node.Schema}.` : "";
    const alias = node.Alias && node.Alias !== relation ? ` ${node.Alias}` : "";
    title += ` on ${schema}${relation}${alias}`;
  } else if (node["CTE Name"]) {
    title += ` on ${node["CTE Name"]}`;
  } else if (node["Function Name"]) {
    title += ` on ${node["Function Name"]}`;
  } else if (node.Alias && node["Node Type"] === "Subquery Scan") {
    title += ` on ${node.Alias}`;
  }
  if (node["Subplan Name"]) title = `${node["Subplan Name"]}: ${title}`;
  return title;
}

function num(value: unknown) {
  return typeof value === "number" ? value : 0;
}

function formatRows(rows: number) {
  return Math.round(rows).toLocaleString();
}

function formatMs(ms: number) {
  return ms >= 100 ? Math.round(ms).toLocaleString() : ms.toFixed(ms >= 10 ? 1 : 2);
}
