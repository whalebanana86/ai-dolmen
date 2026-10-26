import { ReactNode, useState } from "react";
import { Column, Introspection, Schema, Table, View } from "./api";

interface Props {
  introspection: Introspection | null;
  loading: boolean;
  onRefresh: () => void;
  /** Called when a table or view is double-clicked. */
  onOpen: (schema: string, name: string) => void;
}

/**
 * The database structure as a tree: schemas, their tables, views, functions,
 * sequences and enums, and each table's columns, constraints and indexes.
 */
export function SchemaTree({ introspection, loading, onRefresh, onOpen }: Props) {
  const [filter, setFilter] = useState("");
  // Node keys the user toggled; the default schema starts open, everything else closed.
  const [toggled, setToggled] = useState<Set<string>>(new Set());
  const defaultSchema = introspection?.defaultSchema;

  const isOpen = (key: string, openByDefault = false) => toggled.has(key) !== openByDefault;
  const toggle = (key: string) =>
    setToggled((current) => {
      const next = new Set(current);
      if (!next.delete(key)) next.add(key);
      return next;
    });

  const query = filter.trim().toLowerCase();
  const matches = (name: string) => name.toLowerCase().includes(query);

  return (
    <aside className="schema-tree">
      <div className="schema-tree-header">
        <input
          className="schema-filter"
          placeholder="Filter"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <button onClick={onRefresh} disabled={loading} title="Reload the database structure">
          ↻
        </button>
      </div>
      <div className="schema-tree-body">
        {!introspection && <p className="muted tree-message">{loading ? "Loading…" : "Not loaded"}</p>}
        {introspection?.database.schemas.map((schema) => {
          const key = `schema:${schema.name}`;
          const filtered = query ? filterSchema(schema, matches) : schema;
          if (query && isEmpty(filtered)) return null;
          // While filtering, open everything so matches are visible.
          const open = query !== "" || isOpen(key, schema.name === defaultSchema);
          return (
            <div key={key}>
              <TreeRow
                depth={0}
                open={open}
                onToggle={() => toggle(key)}
                badge="S"
                label={schema.name}
              />
              {open && (
                <SchemaChildren
                  schema={filtered}
                  forceOpen={query !== ""}
                  isOpen={isOpen}
                  toggle={toggle}
                  onOpen={onOpen}
                />
              )}
            </div>
          );
        })}
      </div>
    </aside>
  );
}

interface ChildrenProps {
  schema: Schema;
  forceOpen: boolean;
  isOpen: (key: string, openByDefault?: boolean) => boolean;
  toggle: (key: string) => void;
  onOpen: (schema: string, name: string) => void;
}

function SchemaChildren({ schema, forceOpen, isOpen, toggle, onOpen }: ChildrenProps) {
  const groupKey = (group: string) => `group:${schema.name}:${group}`;
  const group = (name: string, count: number, children: () => ReactNode, openByDefault = false) => {
    if (count === 0) return null;
    const key = groupKey(name);
    const open = forceOpen || isOpen(key, openByDefault);
    return (
      <div key={key}>
        <TreeRow depth={1} open={open} onToggle={() => toggle(key)} label={name} detail={String(count)} />
        {open && children()}
      </div>
    );
  };

  return (
    <>
      {group("Tables", schema.tables.length, () =>
        schema.tables.map((table) => (
          <RelationNode
            key={table.name}
            schema={schema.name}
            relation={table}
            badge={table.kind === "foreign" ? "F" : "T"}
            isOpen={isOpen}
            toggle={toggle}
            onOpen={onOpen}
          />
        )),
        true,
      )}
      {group("Views", schema.views.length, () =>
        schema.views.map((view) => (
          <RelationNode
            key={view.name}
            schema={schema.name}
            relation={view}
            badge={view.materialized ? "M" : "V"}
            isOpen={isOpen}
            toggle={toggle}
            onOpen={onOpen}
          />
        )),
      )}
      {group("Functions", schema.functions.length, () =>
        schema.functions.map((fn) => (
          <TreeRow
            key={`${fn.name}(${fn.arguments})`}
            depth={2}
            badge={fn.kind === "procedure" ? "P" : "ƒ"}
            label={fn.name}
            detail={`(${fn.arguments})`}
            title={fn.definition}
          />
        )),
      )}
      {group("Sequences", schema.sequences.length, () =>
        schema.sequences.map((sequence) => (
          <TreeRow key={sequence.name} depth={2} badge="#" label={sequence.name} />
        )),
      )}
      {group("Enums", schema.enums.length, () =>
        schema.enums.map((type) => (
          <TreeRow
            key={type.name}
            depth={2}
            badge="E"
            label={type.name}
            detail={type.labels.join(", ")}
          />
        )),
      )}
    </>
  );
}

interface RelationProps {
  schema: string;
  relation: Table | View;
  badge: string;
  isOpen: (key: string) => boolean;
  toggle: (key: string) => void;
  onOpen: (schema: string, name: string) => void;
}

function RelationNode({ schema, relation, badge, isOpen, toggle, onOpen }: RelationProps) {
  const key = `relation:${schema}:${relation.name}`;
  const open = isOpen(key);
  const table = "constraints" in relation ? relation : null;
  const primaryKey = new Set(table ? primaryKeyColumns(table) : []);

  return (
    <div>
      <TreeRow
        depth={2}
        open={open}
        onToggle={() => toggle(key)}
        onDoubleClick={() => onOpen(schema, relation.name)}
        badge={badge}
        label={relation.name}
        title={[relation.comment, "Double-click to query"].filter(Boolean).join("\n")}
      />
      {open && (
        <>
          {relation.columns.map((column) => (
            <TreeRow
              key={column.name}
              depth={3}
              badge={primaryKey.has(column.name) ? "PK" : ""}
              label={column.name}
              detail={column.dataType + (column.nullable ? "" : " not null")}
              title={columnTitle(column)}
            />
          ))}
          {table?.constraints
            .filter((constraint) => constraint.kind !== "primaryKey")
            .map((constraint) => (
              <TreeRow
                key={constraint.name}
                depth={3}
                badge={CONSTRAINT_BADGES[constraint.kind]}
                label={constraint.name}
                title={constraint.definition}
              />
            ))}
          {table?.indexes.map((index) => (
            <TreeRow key={index.name} depth={3} badge="IX" label={index.name} title={index.definition} />
          ))}
        </>
      )}
    </div>
  );
}

const CONSTRAINT_BADGES = {
  primaryKey: "PK",
  unique: "UQ",
  check: "CK",
  foreignKey: "FK",
  exclusion: "EX",
} as const;

interface RowProps {
  depth: number;
  label: string;
  detail?: string;
  /** An empty badge keeps the space, so badged and plain siblings line up. */
  badge?: string;
  title?: string;
  /** Leaf rows have no `open`. */
  open?: boolean;
  onToggle?: () => void;
  onDoubleClick?: () => void;
}

function TreeRow({ depth, label, detail, badge, title, open, onToggle, onDoubleClick }: RowProps) {
  return (
    <div
      className="tree-row"
      style={{ paddingLeft: 6 + depth * 14 }}
      onClick={onToggle}
      onDoubleClick={onDoubleClick}
      title={title}
    >
      <span className="tree-caret">{open === undefined ? "" : open ? "▾" : "▸"}</span>
      {badge !== undefined && <span className={badge ? "tree-badge" : "tree-badge empty"}>{badge}</span>}
      <span className="tree-label">{label}</span>
      {detail && <span className="tree-detail">{detail}</span>}
    </div>
  );
}

/** Column names inside `PRIMARY KEY (...)`, unquoted. */
function primaryKeyColumns(table: Table): string[] {
  const definition = table.constraints.find((c) => c.kind === "primaryKey")?.definition;
  const inner = definition?.match(/\((.*)\)/)?.[1];
  return inner ? inner.split(",").map((name) => name.trim().replace(/^"(.*)"$/, "$1")) : [];
}

function columnTitle(column: Column) {
  return [
    `${column.name} ${column.dataType}${column.nullable ? "" : " NOT NULL"}`,
    column.identity && `GENERATED ${column.identity === "always" ? "ALWAYS" : "BY DEFAULT"} AS IDENTITY`,
    column.generated && `GENERATED ALWAYS AS (${column.generated})`,
    column.default && `DEFAULT ${column.default}`,
    column.comment,
  ]
    .filter(Boolean)
    .join("\n");
}

function filterSchema(schema: Schema, matches: (name: string) => boolean): Schema {
  return {
    ...schema,
    tables: schema.tables.filter((t) => matches(t.name)),
    views: schema.views.filter((v) => matches(v.name)),
    functions: schema.functions.filter((f) => matches(f.name)),
    sequences: schema.sequences.filter((s) => matches(s.name)),
    enums: schema.enums.filter((e) => matches(e.name)),
  };
}

function isEmpty(schema: Schema) {
  return (
    schema.tables.length + schema.views.length + schema.functions.length + schema.sequences.length + schema.enums.length === 0
  );
}
