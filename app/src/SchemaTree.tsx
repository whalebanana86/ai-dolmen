import { MouseEvent, ReactNode, useCallback, useState } from "react";
import {
  Column,
  Constraint,
  Index,
  Introspection,
  ObjectRef,
  Schema,
  SchemaObject,
  SqlFunction,
  Table,
  View,
} from "./api";
import { ContextMenu, MenuItem } from "./ContextMenu";
import type { Messages } from "./i18n/en";
import { useT } from "./settings";
import { quoteIdent } from "./sqlIdent";

type NewObject = "table" | "view" | "function";

/** The menu item that starts a new object of the kind. */
function createItem(schema: string, type: NewObject, actions: TreeActions, t: Messages): MenuItem {
  const label = { table: t.tree.menu.newTable, view: t.tree.menu.newView, function: t.tree.menu.newFunction }[type];
  return { label, onSelect: () => actions.create(schema, type) };
}

/** What the tree's menus can do in the workspace. */
export interface TreeActions {
  /** Adds SQL at the end of the editor, and runs it when `run` is set. */
  insert: (sql: string, run: boolean) => void;
  /** Adds the object's CREATE statement to the editor. */
  showDdl: (schema: string, object: SchemaObject) => void;
  /** Opens a table's, view's or function's details. */
  showDetail: (target: ObjectRef) => void;
  /** Opens the ERD of a schema, or of one table and its neighbors. */
  showErd: (schema: string, table: string | null) => void;
  /** Starts writing a new table, view or function in the schema. */
  create: (schema: string, type: NewObject) => void;
  /** Saves a table's or view's rows to a file. */
  exportData: (schema: string, relation: string) => void;
  /** Appends a file's rows to a table. */
  importData: (schema: string, table: Table) => void;
  refresh: () => void;
}

interface Props {
  introspection: Introspection | null;
  loading: boolean;
  actions: TreeActions;
  /** In px. */
  width: number;
}

type OpenMenu = (event: MouseEvent, items: MenuItem[]) => void;

/**
 * The database structure as a tree: schemas, their tables, views, functions,
 * sequences and enums, and each table's columns, constraints and indexes.
 * Right-click a node for its actions; double-click a table or view to query it.
 */
export function SchemaTree({ introspection, loading, actions, width }: Props) {
  const t = useT();
  const [filter, setFilter] = useState("");
  // Node keys the user toggled; the default schema starts open, everything else closed.
  const [toggled, setToggled] = useState<Set<string>>(new Set());
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  const defaultSchema = introspection?.defaultSchema ?? null;

  const isOpen = (key: string, openByDefault = false) => toggled.has(key) !== openByDefault;
  const toggle = (key: string) =>
    setToggled((current) => {
      const next = new Set(current);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  const openMenu: OpenMenu = (event, items) => {
    event.preventDefault();
    setMenu({ x: event.clientX, y: event.clientY, items });
  };
  const closeMenu = useCallback(() => setMenu(null), []);

  const query = filter.trim().toLowerCase();
  const matches = (name: string) => name.toLowerCase().includes(query);
  const sql = new SqlFor(defaultSchema);

  return (
    <aside className="schema-tree" style={{ width }}>
      <div className="schema-tree-header">
        <input
          className="schema-filter"
          placeholder={t.tree.filter}
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <button onClick={actions.refresh} disabled={loading} title={t.tree.reload}>
          ↻
        </button>
      </div>
      <div className="schema-tree-body">
        {!introspection && (
          <p className="muted tree-message">{loading ? t.common.loading : t.tree.notLoaded}</p>
        )}
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
                onContextMenu={(e) =>
                  openMenu(e, [
                    { label: t.tree.menu.showErd, onSelect: () => actions.showErd(schema.name, null) },
                    { separator: true },
                    createItem(schema.name, "table", actions, t),
                    createItem(schema.name, "view", actions, t),
                    createItem(schema.name, "function", actions, t),
                    { separator: true },
                    copyItem(t.tree.menu.copyName, schema.name),
                    { label: t.tree.menu.refresh, onSelect: actions.refresh },
                    { separator: true },
                    {
                      label: t.tree.menu.dropSchema,
                      danger: true,
                      onSelect: () => actions.insert(`DROP SCHEMA ${quoteIdent(schema.name)};`, false),
                    },
                  ])
                }
                badge="S"
                label={schema.name}
              />
              {open && (
                <SchemaChildren
                  schema={filtered}
                  forceOpen={query !== ""}
                  isOpen={isOpen}
                  toggle={toggle}
                  openMenu={openMenu}
                  actions={actions}
                  sql={sql}
                />
              )}
            </div>
          );
        })}
      </div>
      {menu && <ContextMenu {...menu} onClose={closeMenu} />}
    </aside>
  );
}

interface ChildrenProps {
  schema: Schema;
  forceOpen: boolean;
  isOpen: (key: string, openByDefault?: boolean) => boolean;
  toggle: (key: string) => void;
  openMenu: OpenMenu;
  actions: TreeActions;
  sql: SqlFor;
}

function SchemaChildren({ schema, forceOpen, isOpen, toggle, openMenu, actions, sql }: ChildrenProps) {
  const t = useT();
  const group = (
    name: string,
    count: number,
    children: () => ReactNode,
    openByDefault = false,
    creates: NewObject | null = null,
  ) => {
    if (count === 0) return null;
    const key = `group:${schema.name}:${name}`;
    const open = forceOpen || isOpen(key, openByDefault);
    return (
      <div key={key}>
        <TreeRow
          depth={1}
          open={open}
          onToggle={() => toggle(key)}
          onContextMenu={(e) =>
            openMenu(e, [
              ...(creates ? [createItem(schema.name, creates, actions, t), { separator: true as const }] : []),
              { label: t.tree.menu.refresh, onSelect: actions.refresh },
            ])
          }
          label={name}
          detail={String(count)}
        />
        {open && children()}
      </div>
    );
  };
  const relationProps = { schema: schema.name, isOpen, toggle, openMenu, actions, sql };

  return (
    <>
      {group(
        t.tree.groups.tables,
        schema.tables.length,
        () =>
          schema.tables.map((table) => (
            <RelationNode
              key={table.name}
              relation={table}
              badge={table.kind === "foreign" ? "F" : "T"}
              {...relationProps}
            />
          )),
        true,
        "table",
      )}
      {group(t.tree.groups.views, schema.views.length, () =>
        schema.views.map((view) => (
          <RelationNode
            key={view.name}
            relation={view}
            badge={view.materialized ? "M" : "V"}
            {...relationProps}
          />
        )),
        false,
        "view",
      )}
      {group(t.tree.groups.functions, schema.functions.length, () =>
        schema.functions.map((fn) => (
          <TreeRow
            key={`${fn.name}(${fn.arguments})`}
            depth={2}
            badge={fn.kind === "procedure" ? "P" : "ƒ"}
            label={fn.name}
            detail={`(${fn.arguments})`}
            title={fn.definition}
            onContextMenu={(e) => openMenu(e, functionMenu(schema.name, fn, actions, sql, t))}
          />
        )),
        false,
        "function",
      )}
      {group(t.tree.groups.sequences, schema.sequences.length, () =>
        schema.sequences.map((sequence) => {
          const name = sql.name(schema.name, sequence.name);
          return (
            <TreeRow
              key={sequence.name}
              depth={2}
              badge="#"
              label={sequence.name}
              onContextMenu={(e) =>
                openMenu(e, [
                  {
                    label: t.tree.menu.showCurrentValue,
                    onSelect: () => actions.insert(`SELECT last_value, is_called FROM ${name};`, true),
                  },
                  {
                    label: t.tree.menu.showDdl,
                    onSelect: () => actions.showDdl(schema.name, { type: "sequence", ...sequence }),
                  },
                  { separator: true },
                  copyItem(t.tree.menu.copyName, sequence.name),
                  copyItem(t.tree.menu.copyQualifiedName, sql.qualified(schema.name, sequence.name)),
                  { separator: true },
                  dropItem(`DROP SEQUENCE ${name};`, actions, t),
                ])
              }
            />
          );
        }),
      )}
      {group(t.tree.groups.enums, schema.enums.length, () =>
        schema.enums.map((type) => (
          <TreeRow
            key={type.name}
            depth={2}
            badge="E"
            label={type.name}
            detail={type.labels.join(", ")}
            onContextMenu={(e) =>
              openMenu(e, [
                { label: t.tree.menu.showDdl, onSelect: () => actions.showDdl(schema.name, { type: "enum", ...type }) },
                { separator: true },
                copyItem(t.tree.menu.copyName, type.name),
                copyItem(t.tree.menu.copyQualifiedName, sql.qualified(schema.name, type.name)),
                { separator: true },
                dropItem(`DROP TYPE ${sql.name(schema.name, type.name)};`, actions, t),
              ])
            }
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
  openMenu: OpenMenu;
  actions: TreeActions;
  sql: SqlFor;
}

function RelationNode({ schema, relation, badge, isOpen, toggle, openMenu, actions, sql }: RelationProps) {
  const t = useT();
  const key = `relation:${schema}:${relation.name}`;
  const open = isOpen(key);
  const table = "constraints" in relation ? relation : null;
  const primaryKey = new Set(table ? primaryKeyColumns(table) : []);
  const name = sql.name(schema, relation.name);

  return (
    <div>
      <TreeRow
        depth={2}
        open={open}
        onToggle={() => toggle(key)}
        onDoubleClick={() => actions.insert(`SELECT * FROM ${name} LIMIT 1000;`, true)}
        onContextMenu={(e) => openMenu(e, relationMenu(schema, relation, actions, sql, t))}
        badge={badge}
        label={relation.name}
        title={[relation.comment, t.tree.doubleClickToQuery].filter(Boolean).join("\n")}
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
              onContextMenu={(e) =>
                openMenu(e, [
                  {
                    label: t.tree.menu.showValueCounts,
                    onSelect: () => {
                      const col = quoteIdent(column.name);
                      actions.insert(
                        `SELECT ${col}, count(*) FROM ${name}\nGROUP BY ${col}\nORDER BY count(*) DESC\nLIMIT 100;`,
                        true,
                      );
                    },
                  },
                  { separator: true },
                  copyItem(t.tree.menu.copyName, column.name),
                ])
              }
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
                onContextMenu={(e) => openMenu(e, constraintMenu(name, constraint, actions, t))}
              />
            ))}
          {table?.indexes.map((index) => (
            <TreeRow
              key={index.name}
              depth={3}
              badge="IX"
              label={index.name}
              title={index.definition}
              onContextMenu={(e) => openMenu(e, indexMenu(schema, index, actions, sql, t))}
            />
          ))}
        </>
      )}
    </div>
  );
}

function relationMenu(
  schema: string,
  relation: Table | View,
  actions: TreeActions,
  sql: SqlFor,
  t: Messages,
): MenuItem[] {
  const name = sql.name(schema, relation.name);
  const isTable = "constraints" in relation;
  const materialized = !isTable && relation.materialized;
  const kind = isTable ? "TABLE" : materialized ? "MATERIALIZED VIEW" : "VIEW";
  const columns = relation.columns.map((c) => quoteIdent(c.name)).join(", ");
  // Identity ALWAYS and generated columns can't be given values.
  const insertable = relation.columns.filter((c) => c.identity !== "always" && !c.generated);

  return [
    {
      label: t.tree.menu.showDetail,
      onSelect: () => actions.showDetail({ type: isTable ? "table" : "view", schema, name: relation.name }),
    },
    ...(isTable
      ? [{ label: t.tree.menu.showInErd, onSelect: () => actions.showErd(schema, relation.name) }]
      : []),
    createItem(schema, isTable ? "table" : "view", actions, t),
    { separator: true },
    { label: t.tree.menu.selectRows, onSelect: () => actions.insert(`SELECT * FROM ${name} LIMIT 1000;`, true) },
    { label: t.tree.menu.countRows, onSelect: () => actions.insert(`SELECT count(*) FROM ${name};`, true) },
    { label: t.tree.menu.exportData, onSelect: () => actions.exportData(schema, relation.name) },
    ...(isTable ? [{ label: t.tree.menu.importData, onSelect: () => actions.importData(schema, relation) }] : []),
    { separator: true },
    {
      label: t.tree.menu.generateSelect,
      onSelect: () => actions.insert(`SELECT ${columns}\nFROM ${name};`, false),
    },
    ...(isTable
      ? [
          {
            label: t.tree.menu.generateInsert,
            onSelect: () =>
              actions.insert(
                `INSERT INTO ${name} (${insertable.map((c) => quoteIdent(c.name)).join(", ")})\n` +
                  `VALUES (${insertable.map(() => "NULL").join(", ")});`,
                false,
              ),
          },
        ]
      : []),
    ...(materialized
      ? [
          {
            label: t.tree.menu.generateRefresh,
            onSelect: () => actions.insert(`REFRESH MATERIALIZED VIEW ${name};`, false),
          },
        ]
      : []),
    { separator: true },
    copyItem(t.tree.menu.copyName, relation.name),
    copyItem(t.tree.menu.copyQualifiedName, sql.qualified(schema, relation.name)),
    { separator: true },
    ...(isTable
      ? [
          {
            label: t.tree.menu.truncate,
            danger: true,
            onSelect: () => actions.insert(`TRUNCATE TABLE ${name};`, false),
          },
        ]
      : []),
    dropItem(`DROP ${kind} ${name};`, actions, t),
  ];
}

function functionMenu(schema: string, fn: SqlFunction, actions: TreeActions, sql: SqlFor, t: Messages): MenuItem[] {
  const signature = `${sql.name(schema, fn.name)}(${fn.arguments})`;
  return [
    {
      label: t.tree.menu.showDetail,
      onSelect: () => actions.showDetail({ type: "function", schema, name: fn.name, arguments: fn.arguments }),
    },
    createItem(schema, "function", actions, t),
    { separator: true },
    copyItem(t.tree.menu.copyName, fn.name),
    copyItem(t.tree.menu.copySignature, signature),
    { separator: true },
    dropItem(`DROP ${fn.kind === "procedure" ? "PROCEDURE" : "FUNCTION"} ${signature};`, actions, t),
  ];
}

function constraintMenu(table: string, constraint: Constraint, actions: TreeActions, t: Messages): MenuItem[] {
  return [
    copyItem(t.tree.menu.copyDefinition, constraint.definition),
    copyItem(t.tree.menu.copyName, constraint.name),
    { separator: true },
    dropItem(`ALTER TABLE ${table} DROP CONSTRAINT ${quoteIdent(constraint.name)};`, actions, t),
  ];
}

function indexMenu(schema: string, index: Index, actions: TreeActions, sql: SqlFor, t: Messages): MenuItem[] {
  return [
    copyItem(t.tree.menu.copyDefinition, `${index.definition};`),
    copyItem(t.tree.menu.copyName, index.name),
    { separator: true },
    dropItem(`DROP INDEX ${sql.name(schema, index.name)};`, actions, t),
  ];
}

function copyItem(label: string, text: string): MenuItem {
  return {
    label,
    onSelect: () => {
      navigator.clipboard.writeText(text).catch((error) => console.error("copy failed", error));
    },
  };
}

/** Destructive statements go into the editor for review; they never run from the menu. */
function dropItem(statement: string, actions: TreeActions, t: Messages): MenuItem {
  return { label: t.tree.menu.drop, danger: true, onSelect: () => actions.insert(statement, false) };
}

/** Writes object names relative to the default schema, quoting where needed. */
class SqlFor {
  constructor(private defaultSchema: string | null) {}

  qualified(schema: string, name: string) {
    return `${quoteIdent(schema)}.${quoteIdent(name)}`;
  }

  /** The shortest name that reaches the object: unqualified in the default schema. */
  name(schema: string, name: string) {
    return schema === this.defaultSchema ? quoteIdent(name) : this.qualified(schema, name);
  }
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
  onContextMenu?: (event: MouseEvent) => void;
}

function TreeRow({ depth, label, detail, badge, title, open, onToggle, onDoubleClick, onContextMenu }: RowProps) {
  return (
    <div
      className="tree-row"
      style={{ paddingLeft: 4 + depth * 16 }}
      onClick={onToggle}
      onDoubleClick={onDoubleClick}
      onContextMenu={onContextMenu}
      title={title}
    >
      <span className="tree-caret">
        {open !== undefined && (
          <svg viewBox="0 0 16 16" className={open ? "open" : undefined} aria-hidden>
            <path d="M6 3.5 10.5 8 6 12.5" />
          </svg>
        )}
      </span>
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
    column.generated &&
      `GENERATED ALWAYS AS (${column.generated.expression}) ${column.generated.stored ? "STORED" : "VIRTUAL"}`,
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
