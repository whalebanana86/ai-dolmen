import { useEffect, useMemo, useState } from "react";
import {
  Background,
  BackgroundVariant,
  BaseEdge,
  Controls,
  type Edge,
  EdgeLabelRenderer,
  type EdgeProps,
  Handle,
  MiniMap,
  type Node,
  type NodeProps,
  Panel,
  Position,
  ReactFlow,
  ReactFlowProvider,
  getSmoothStepPath,
  useNodesState,
  useReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { Graph, layout } from "@dagrejs/dagre";
import { ForeignKey, Introspection, ObjectRef, Table } from "./api";
import { primaryKeyColumns } from "./sqlIdent";
import { useSettings, useT } from "./settings";

interface Props {
  introspection: Introspection;
  /** The schema to show first; null for all of them. */
  schema: string | null;
  /** A table in `schema` to show with its neighbors only. */
  focus: string | null;
  onOpenDetail: (target: ObjectRef) => void;
  onClose: () => void;
}

/** Tables as boxes of columns, with an arrow from each foreign key to the table it references. */
export function ErdView(props: Props) {
  return (
    <ReactFlowProvider>
      <Erd {...props} />
    </ReactFlowProvider>
  );
}

type TableData = {
  schema: string;
  table: Table;
  label: string;
  primaryKey: string[];
  foreignKeyColumns: string[];
  /** From another schema, shown because a table here references it or is referenced by it. */
  outside: boolean;
};
type TableNode = Node<TableData, "table">;

type RelationshipData = {
  fk: ForeignKey;
  /** The referenced table's palette slot, 1 to PALETTE_SIZE. */
  color: number;
  /** Where the line turns, from the layout; null once one of its tables was moved. */
  route: Route | null;
  /** For a table that references itself: how far right of the box the loop goes. */
  loop: number | null;
};
type RelationshipEdge = Edge<RelationshipData, "relationship">;

/**
 * An orthogonal path: it turns vertically at each x in `turns`, one per gap
 * between columns of tables, and crosses each column in between at the
 * matching y in `lanes`, which the layout keeps clear of tables.
 */
type Route = { turns: number[]; lanes: number[] };

/** Referenced tables, by the order of their ids, cycle through this many colors (App.css --erd-N). */
const PALETTE_SIZE = 8;
const NODE_WIDTH = 260;
const HEADER_HEIGHT = 34;
const ROW_HEIGHT = 22;
const RANK_GAP = 120;

function Erd({ introspection, schema: initialSchema, focus: initialFocus, onOpenDetail, onClose }: Props) {
  const t = useT();
  const { settings } = useSettings();
  const schemas = introspection.database.schemas.filter((s) => s.tables.length > 0).map((s) => s.name);
  const [schema, setSchema] = useState<string | null>(
    initialSchema && schemas.includes(initialSchema) ? initialSchema : (schemas[0] ?? null),
  );
  const [focus, setFocus] = useState<string | null>(initialFocus);
  /** The table whose relationships stand out; the rest fade. */
  const [selected, setSelected] = useState<string | null>(null);
  /** Tables the user dragged, whose lines no longer follow the layout's lanes. */
  const [moved, setMoved] = useState<Set<string>>(new Set());

  const diagram = useMemo(() => buildDiagram(introspection, schema, focus), [introspection, schema, focus]);
  const [nodes, setNodes, onNodesChange] = useNodesState<TableNode>(diagram.nodes);
  const { fitView } = useReactFlow();

  useEffect(() => {
    setNodes(diagram.nodes);
    setSelected(null);
    setMoved(new Set());
    // Wait for the new nodes to be measured before fitting them in.
    requestAnimationFrame(() => fitView({ padding: 0.1, duration: 200 }));
  }, [diagram]);

  const related = useMemo(() => {
    if (!selected) return null;
    const ids = new Set([selected]);
    for (const e of diagram.edges) if (e.source === selected || e.target === selected) ids.add(e.source).add(e.target);
    return ids;
  }, [diagram, selected]);

  const shownNodes = useMemo(
    () => nodes.map((node) => (related && !related.has(node.id) ? { ...node, className: "dimmed" } : node)),
    [nodes, related],
  );
  const edges = useMemo(
    () =>
      diagram.edges.map((edge): RelationshipEdge => {
        const touches = selected !== null && (edge.source === selected || edge.target === selected);
        const route = moved.has(edge.source) || moved.has(edge.target) ? null : edge.data!.route;
        return {
          ...edge,
          data: { ...edge.data!, route },
          animated: touches,
          className: selected === null ? undefined : touches ? "highlighted" : "dimmed",
          zIndex: touches ? 1 : 0,
        };
      }),
    [diagram, selected, moved],
  );

  return (
    <div className="dialog-backdrop erd-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="dialog erd-dialog" role="dialog" aria-label={t.erd.title}>
        <header className="side-panel-header">
          <strong>{t.erd.title}</strong>
          <label className="erd-schema">
            {t.erd.schema}
            <select
              value={schema ?? ""}
              onChange={(e) => {
                setSchema(e.target.value || null);
                setFocus(null);
              }}
            >
              {schemas.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
              <option value="">{t.erd.allSchemas}</option>
            </select>
          </label>
          {focus && (
            <label className="checkbox">
              <input type="checkbox" checked onChange={() => setFocus(null)} />
              {t.erd.onlyRelated(focus)}
            </label>
          )}
          <span className="muted erd-help">{t.erd.help}</span>
          <span className="spacer" />
          <button onClick={onClose} title={t.common.close}>
            ✕
          </button>
        </header>
        <div className="erd-canvas">
          {nodes.length === 0 ? (
            <p className="muted erd-empty">{t.erd.empty}</p>
          ) : (
            <ReactFlow
              nodes={shownNodes}
              edges={edges}
              nodeTypes={nodeTypes}
              edgeTypes={edgeTypes}
              onNodesChange={onNodesChange}
              onNodeDragStart={(_, node) => setMoved((current) => new Set(current).add(node.id))}
              onNodeClick={(_, node) => setSelected(node.id)}
              onPaneClick={() => setSelected(null)}
              onNodeDoubleClick={(_, node) =>
                onOpenDetail({ type: "table", schema: node.data.schema, name: node.data.table.name })
              }
              colorMode={settings.theme}
              minZoom={0.1}
              maxZoom={2}
              nodesConnectable={false}
              edgesFocusable={false}
              onlyRenderVisibleElements
              proOptions={{ hideAttribution: true }}
              fitView
            >
              <Background variant={BackgroundVariant.Dots} gap={18} size={1} />
              <Controls showInteractive={false} />
              <MiniMap pannable zoomable nodeStrokeWidth={2} />
              {diagram.legend.length > 0 && (
                <Panel position="top-right" className="erd-legend">
                  <p className="muted">{t.erd.legend}</p>
                  {diagram.legend.map((entry) => (
                    <button
                      key={entry.id}
                      className={selected === entry.id ? "selected" : undefined}
                      onClick={() => setSelected(selected === entry.id ? null : entry.id)}
                    >
                      <span className="erd-swatch" style={{ background: `var(--erd-${entry.color})` }} />
                      {entry.label}
                    </button>
                  ))}
                </Panel>
              )}
            </ReactFlow>
          )}
        </div>
      </div>
    </div>
  );
}

function TableBox({ data }: NodeProps<TableNode>) {
  return (
    <div className={`erd-table${data.outside ? " outside" : ""}`} title={data.table.comment ?? undefined}>
      <div className="erd-table-name">{data.label}</div>
      {data.table.columns.map((column) => {
        const key = data.primaryKey.includes(column.name)
          ? "PK"
          : data.foreignKeyColumns.includes(column.name)
            ? "FK"
            : "";
        return (
          <div key={column.name} className="erd-column">
            <Handle type="target" position={Position.Left} id={`${column.name}:in`} isConnectable={false} />
            <span className={`erd-key ${key.toLowerCase()}`}>{key}</span>
            <span className="erd-column-name">{column.name}</span>
            <span className="erd-column-type">{column.dataType}</span>
            <Handle type="source" position={Position.Right} id={`${column.name}:out`} isConnectable={false} />
            {/* A table that references itself loops back on its right side. */}
            <Handle type="target" position={Position.Right} id={`${column.name}:self`} isConnectable={false} />
          </div>
        );
      })}
    </div>
  );
}

/** A foreign key: a line in its referenced table's color, ending in an arrow at the referenced column. */
function RelationshipLine({ id, sourceX, sourceY, targetX, targetY, data }: EdgeProps<RelationshipEdge>) {
  const { fk, color, route, loop } = data!;
  const stroke = `var(--erd-${color})`;
  let path: string;
  let label: [number, number];
  if (loop !== null) {
    const out = Math.max(sourceX, targetX) + loop;
    path = rounded([[sourceX, sourceY], [out, sourceY], [out, targetY], [targetX, targetY]]);
    label = [out, (sourceY + targetY) / 2];
  } else if (route && targetX > sourceX) {
    const points: [number, number][] = [[sourceX, sourceY]];
    let y = sourceY;
    route.turns.forEach((x, i) => {
      const next = i < route.lanes.length ? route.lanes[i] : targetY;
      points.push([x, y], [x, next]);
      y = next;
    });
    points.push([targetX, targetY]);
    path = rounded(points);
    label = [route.turns[0], (sourceY + (route.lanes[0] ?? targetY)) / 2];
  } else {
    const [smooth, x, y] = getSmoothStepPath({
      sourceX,
      sourceY,
      sourcePosition: Position.Right,
      targetX,
      targetY,
      targetPosition: Position.Left,
      borderRadius: 8,
    });
    path = smooth;
    label = [x, y];
  }
  // The arrow points into the referenced column: rightwards, or leftwards on a loop.
  const tip = loop !== null ? 1 : -1;
  const arrow = `M ${targetX} ${targetY} L ${targetX - tip * 9} ${targetY - 4.5} L ${targetX - tip * 9} ${targetY + 4.5} Z`;

  return (
    <>
      <BaseEdge id={id} path={path} style={{ stroke }} interactionWidth={14} />
      <path className="erd-arrow" d={arrow} style={{ fill: stroke }} />
      {fk.columns.length > 1 && (
        <EdgeLabelRenderer>
          <div
            className="erd-edge-label"
            style={{ transform: `translate(-50%, -50%) translate(${label[0]}px, ${label[1]}px)` }}
          >
            ({fk.columns.join(", ")})
          </div>
        </EdgeLabelRenderer>
      )}
    </>
  );
}

const nodeTypes = { table: TableBox };
const edgeTypes = { relationship: RelationshipLine };

/** A path through `points` with its corners rounded. */
function rounded(input: [number, number][], radius = 8): string {
  const points = input.filter(
    ([x, y], i) => i === 0 || Math.abs(x - input[i - 1][0]) > 0.5 || Math.abs(y - input[i - 1][1]) > 0.5,
  );
  let d = `M ${points[0][0]} ${points[0][1]}`;
  for (let i = 1; i < points.length - 1; i++) {
    const [px, py] = points[i - 1];
    const [x, y] = points[i];
    const [nx, ny] = points[i + 1];
    const r = Math.min(radius, Math.hypot(x - px, y - py) / 2, Math.hypot(nx - x, ny - y) / 2);
    const towards = (ax: number, ay: number) => {
      const length = Math.hypot(ax - x, ay - y) || 1;
      return `${x + ((ax - x) / length) * r} ${y + ((ay - y) / length) * r}`;
    };
    d += ` L ${towards(px, py)} Q ${x} ${y} ${towards(nx, ny)}`;
  }
  const [lx, ly] = points[points.length - 1];
  return `${d} L ${lx} ${ly}`;
}

/**
 * The tables in `schema` (all schemas when null), or only `focus` and the
 * tables it shares a foreign key with, plus tables in other schemas they
 * reference or are referenced by, laid out left to right along the keys.
 */
function buildDiagram(
  { database, foreignKeys }: Introspection,
  schema: string | null,
  focus: string | null,
): { nodes: TableNode[]; edges: RelationshipEdge[]; legend: { id: string; label: string; color: number }[] } {
  const id = (s: string, table: string) => `${s}.${table}`;
  const tables = new Map<string, { schema: string; table: Table }>();
  for (const s of database.schemas) for (const table of s.tables) tables.set(id(s.name, table.name), { schema: s.name, table });

  const inScope = (s: string, table: string) =>
    focus !== null ? s === schema && table === focus : schema === null || s === schema;
  const touching = foreignKeys.filter(
    (fk) => inScope(fk.schema, fk.table) || inScope(fk.referencedSchema, fk.referencedTable),
  );
  const shown = new Set<string>();
  for (const [key, { schema: s, table }] of tables) if (inScope(s, table.name)) shown.add(key);
  for (const fk of touching) {
    shown.add(id(fk.schema, fk.table));
    shown.add(id(fk.referencedSchema, fk.referencedTable));
  }
  // With a focus, keys between the neighbors show too.
  const keys = foreignKeys.filter(
    (fk) => shown.has(id(fk.schema, fk.table)) && shown.has(id(fk.referencedSchema, fk.referencedTable)),
  );

  // A referenced table keeps its color whichever schema or focus is shown:
  // slots follow the order of all referenced tables in the database.
  const referenced = [...new Set(foreignKeys.map((fk) => id(fk.referencedSchema, fk.referencedTable)))].sort();
  const colorOf = (table: string) => (referenced.indexOf(table) % PALETTE_SIZE) + 1;
  const label = (key: string) => {
    const entry = tables.get(key);
    return entry && schema !== null && entry.schema === schema ? entry.table.name : key;
  };

  const nodes: TableNode[] = [];
  for (const key of shown) {
    const entry = tables.get(key);
    if (!entry) continue;
    nodes.push({
      id: key,
      type: "table",
      position: { x: 0, y: 0 },
      data: {
        schema: entry.schema,
        table: entry.table,
        label: label(key),
        primaryKey: primaryKeyColumns(entry.table),
        foreignKeyColumns: foreignKeys
          .filter((fk) => fk.schema === entry.schema && fk.table === entry.table.name)
          .flatMap((fk) => fk.columns),
        outside: schema !== null && entry.schema !== schema,
      },
      width: NODE_WIDTH,
    });
  }
  nodes.sort((a, b) => a.id.localeCompare(b.id));

  const edges = keys.map((fk): RelationshipEdge => {
    const source = id(fk.schema, fk.table);
    const target = id(fk.referencedSchema, fk.referencedTable);
    const self = source === target;
    return {
      id: `${source}.${fk.name}`,
      type: "relationship",
      source,
      sourceHandle: `${fk.columns[0]}:out`,
      target,
      targetHandle: `${fk.referencedColumns[0]}:${self ? "self" : "in"}`,
      data: { fk, color: colorOf(target), route: null, loop: null },
      ariaLabel: `${fk.name}: ${fk.table}(${fk.columns.join(", ")}) → ${fk.referencedTable}(${fk.referencedColumns.join(", ")})`,
    };
  });
  place(nodes, edges);

  const legend = [...new Set(edges.map((e) => e.target))]
    .sort((a, b) => colorOf(a) - colorOf(b) || a.localeCompare(b))
    .map((key) => ({ id: key, label: label(key), color: colorOf(key) }));
  return { nodes, edges, legend };
}

/**
 * Lays out the tables that have relationships with dagre, referencing tables
 * left of the ones they reference, and puts the rest in a grid below. Lines
 * that span several columns take the lanes dagre keeps free for them, so they
 * never run through a table, and lines turning in the same gap between
 * columns turn at different points, so they don't run on top of each other.
 */
function place(nodes: TableNode[], edges: RelationshipEdge[]) {
  const height = (node: TableNode) => HEADER_HEIGHT + node.data.table.columns.length * ROW_HEIGHT;
  const byId = new Map(nodes.map((node) => [node.id, node]));
  const rowY = (node: TableNode, column: string) =>
    node.position.y +
    HEADER_HEIGHT +
    Math.max(0, node.data.table.columns.findIndex((c) => c.name === column)) * ROW_HEIGHT +
    ROW_HEIGHT / 2;

  const linked = new Set(edges.flatMap((e) => [e.source, e.target]));
  const graph = new Graph({ multigraph: true });
  graph.setGraph({ rankdir: "LR", nodesep: 40, ranksep: RANK_GAP, marginx: 20, marginy: 20 });
  graph.setDefaultEdgeLabel(() => ({}));
  for (const node of nodes) {
    if (linked.has(node.id)) graph.setNode(node.id, { width: NODE_WIDTH, height: height(node) });
  }
  for (const e of edges) if (e.source !== e.target) graph.setEdge(e.source, e.target, {}, e.id);
  layout(graph);

  let bottom = 0;
  for (const node of nodes) {
    if (!linked.has(node.id)) continue;
    const { x, y } = graph.node(node.id);
    node.position = { x: Math.round(x - NODE_WIDTH / 2), y: Math.round(y - height(node) / 2) };
    bottom = Math.max(bottom, node.position.y + height(node));
  }

  // Columns of tables, by their left edge; every box is NODE_WIDTH wide.
  const columns = [...new Set(nodes.filter((n) => linked.has(n.id)).map((n) => n.position.x))].sort((a, b) => a - b);
  const columnOf = (node: TableNode) => columns.indexOf(node.position.x);
  // Per gap after column g, the lines turning in it, with the height they turn at.
  const gaps = new Map<number, { edge: RelationshipEdge; step: number; y: number }[]>();
  const loops = new Map<string, number>();

  for (const e of edges) {
    const source = byId.get(e.source)!;
    const target = byId.get(e.target)!;
    if (e.source === e.target) {
      const n = loops.get(e.source) ?? 0;
      loops.set(e.source, n + 1);
      e.data!.loop = 18 + n * 10;
      continue;
    }
    const from = columnOf(source);
    const to = columnOf(target);
    if (from < 0 || to <= from) continue; // Points back left: drawn as a plain step line.
    const points: { x: number; y: number }[] = graph.edge(e.source, e.target, e.id)?.points ?? [];
    const lanes: number[] = [];
    for (let c = from + 1; c < to; c++) {
      const center = columns[c] + NODE_WIDTH / 2;
      const lane = points.find((p) => Math.abs(p.x - center) < 2);
      if (!lane) break;
      lanes.push(lane.y);
    }
    if (lanes.length !== to - from - 1) continue;
    const route: Route = { turns: [], lanes };
    e.data!.route = route;
    const start = rowY(source, e.data!.fk.columns[0]);
    for (let step = 0; step < to - from; step++) {
      const y = step === 0 ? start : lanes[step - 1];
      const gap = gaps.get(from + step) ?? [];
      gap.push({ edge: e, step, y });
      gaps.set(from + step, gap);
    }
  }

  // Spread each gap's turns across it, top to bottom in the order the lines arrive.
  for (const [g, turning] of gaps) {
    const left = columns[g] + NODE_WIDTH;
    const width = (columns[g + 1] ?? left + RANK_GAP) - left;
    const pad = Math.min(24, width / 4);
    turning.sort((a, b) => a.y - b.y);
    turning.forEach(({ edge, step }, i) => {
      const x =
        turning.length === 1 ? left + width / 2 : left + pad + (i * (width - 2 * pad)) / (turning.length - 1);
      edge.data!.route!.turns[step] = Math.round(x);
    });
  }

  const alone = nodes.filter((node) => !linked.has(node.id));
  const perRow = Math.max(1, Math.ceil(Math.sqrt(alone.length * 1.5)));
  let y = linked.size > 0 ? bottom + 80 : 20;
  for (let i = 0; i < alone.length; i += perRow) {
    const row = alone.slice(i, i + perRow);
    row.forEach((node, j) => (node.position = { x: 20 + j * (NODE_WIDTH + 40), y }));
    y += Math.max(...row.map(height)) + 40;
  }
}
