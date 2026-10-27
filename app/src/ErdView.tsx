import { useEffect, useMemo, useState } from "react";
import {
  Background,
  BackgroundVariant,
  Controls,
  type Edge,
  Handle,
  MarkerType,
  MiniMap,
  type Node,
  type NodeProps,
  Position,
  ReactFlow,
  ReactFlowProvider,
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

const NODE_WIDTH = 260;
const HEADER_HEIGHT = 34;
const ROW_HEIGHT = 22;

function Erd({ introspection, schema: initialSchema, focus: initialFocus, onOpenDetail, onClose }: Props) {
  const t = useT();
  const { settings } = useSettings();
  const schemas = introspection.database.schemas.filter((s) => s.tables.length > 0).map((s) => s.name);
  const [schema, setSchema] = useState<string | null>(
    initialSchema && schemas.includes(initialSchema) ? initialSchema : (schemas[0] ?? null),
  );
  const [focus, setFocus] = useState<string | null>(initialFocus);
  const [selected, setSelected] = useState<string | null>(null);

  const diagram = useMemo(() => buildDiagram(introspection, schema, focus), [introspection, schema, focus]);
  const [nodes, setNodes, onNodesChange] = useNodesState<TableNode>(diagram.nodes);
  const { fitView } = useReactFlow();

  useEffect(() => {
    setNodes(diagram.nodes);
    setSelected(null);
    // Wait for the new nodes to be measured before fitting them in.
    requestAnimationFrame(() => fitView({ padding: 0.1, duration: 200 }));
  }, [diagram]);

  // The selected table's relationships stand out.
  const edges = useMemo(
    () =>
      diagram.edges.map((edge) =>
        selected && (edge.source === selected || edge.target === selected)
          ? { ...edge, animated: true, className: "highlighted", zIndex: 1 }
          : edge,
      ),
    [diagram, selected],
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
              nodes={nodes}
              edges={edges}
              nodeTypes={nodeTypes}
              onNodesChange={onNodesChange}
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
          </div>
        );
      })}
    </div>
  );
}

const nodeTypes = { table: TableBox };

/**
 * The tables in `schema` (all schemas when null), or only `focus` and the
 * tables it shares a foreign key with, plus tables in other schemas they
 * reference or are referenced by, laid out left to right along the keys.
 */
function buildDiagram(
  { database, foreignKeys }: Introspection,
  schema: string | null,
  focus: string | null,
): { nodes: TableNode[]; edges: Edge[] } {
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

  const nodes: TableNode[] = [];
  for (const key of shown) {
    const entry = tables.get(key);
    if (!entry) continue;
    const outside = schema !== null && entry.schema !== schema;
    nodes.push({
      id: key,
      type: "table",
      position: { x: 0, y: 0 },
      data: {
        schema: entry.schema,
        table: entry.table,
        label: schema === null || outside ? key : entry.table.name,
        primaryKey: primaryKeyColumns(entry.table),
        foreignKeyColumns: foreignKeys
          .filter((fk) => fk.schema === entry.schema && fk.table === entry.table.name)
          .flatMap((fk) => fk.columns),
        outside,
      },
      width: NODE_WIDTH,
    });
  }
  nodes.sort((a, b) => a.id.localeCompare(b.id));
  const edges = keys.map((fk): Edge => edge(fk, id));
  place(nodes, edges);
  return { nodes, edges };
}

function edge(fk: ForeignKey, id: (s: string, t: string) => string): Edge {
  return {
    id: `${fk.schema}.${fk.table}.${fk.name}`,
    source: id(fk.schema, fk.table),
    sourceHandle: `${fk.columns[0]}:out`,
    target: id(fk.referencedSchema, fk.referencedTable),
    targetHandle: `${fk.referencedColumns[0]}:in`,
    type: "smoothstep",
    markerEnd: { type: MarkerType.ArrowClosed, width: 16, height: 16 },
    label: fk.columns.length > 1 ? `(${fk.columns.join(", ")})` : undefined,
    ariaLabel: `${fk.name}: ${fk.table}(${fk.columns.join(", ")}) → ${fk.referencedTable}(${fk.referencedColumns.join(", ")})`,
  };
}

/**
 * Lays out the tables that have relationships with dagre, referencing tables
 * left of the ones they reference, and puts the rest in a grid below.
 */
function place(nodes: TableNode[], edges: Edge[]) {
  // The box's height, from its rows; the box itself grows to fit its content.
  const height = (node: TableNode) => HEADER_HEIGHT + node.data.table.columns.length * ROW_HEIGHT;
  const linked = new Set(edges.flatMap((e) => [e.source, e.target]));
  const graph = new Graph();
  graph.setGraph({ rankdir: "LR", nodesep: 36, ranksep: 110, marginx: 20, marginy: 20 });
  graph.setDefaultEdgeLabel(() => ({}));
  for (const node of nodes) {
    if (linked.has(node.id)) graph.setNode(node.id, { width: NODE_WIDTH, height: height(node) });
  }
  for (const e of edges) if (e.source !== e.target) graph.setEdge(e.source, e.target);
  layout(graph);

  let bottom = 0;
  for (const node of nodes) {
    if (!linked.has(node.id)) continue;
    const { x, y } = graph.node(node.id);
    node.position = { x: x - NODE_WIDTH / 2, y: y - height(node) / 2 };
    bottom = Math.max(bottom, node.position.y + height(node));
  }

  const alone = nodes.filter((node) => !linked.has(node.id));
  const columns = Math.max(1, Math.ceil(Math.sqrt(alone.length * 1.5)));
  let y = linked.size > 0 ? bottom + 80 : 20;
  for (let i = 0; i < alone.length; i += columns) {
    const row = alone.slice(i, i + columns);
    row.forEach((node, j) => (node.position = { x: 20 + j * (NODE_WIDTH + 40), y }));
    y += Math.max(...row.map(height)) + 40;
  }
}
