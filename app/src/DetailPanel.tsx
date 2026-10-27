import { ReactNode, useEffect, useMemo, useState } from "react";
import {
  Column,
  Constraint,
  ForeignKey,
  Index,
  Introspection,
  ObjectEdit,
  ObjectRef,
  Plan,
  SchemaObject,
  SqlFunction,
  Table,
  View,
  errorMessage,
  objects,
  snapshots,
} from "./api";
import { blockerText } from "./ApplyDialog";
import type { Messages } from "./i18n/en";
import { useT } from "./settings";
import { Splitter } from "./Splitter";
import { primaryKeyColumns, quoteIdent, splitNames } from "./sqlIdent";

interface Props {
  sessionId: number;
  introspection: Introspection;
  target: ObjectRef;
  onNavigate: (target: ObjectRef) => void;
  onShowErd: (schema: string, table: string) => void;
  onInsert: (sql: string) => void;
  /** After an edit was applied; `renamed` names the object when its name changed. */
  onChanged: (renamed: ObjectRef | null) => void;
  onClose: () => void;
  width: number;
  onResize: (width: number) => void;
  onResetWidth: () => void;
}

type Found =
  | { type: "table"; schema: string; table: Table }
  | { type: "view"; schema: string; view: View }
  | { type: "function"; schema: string; fn: SqlFunction };

/**
 * A table's, view's or function's details: columns, keys, relationships,
 * constraints and indexes, its DDL, and an editor whose changes are reviewed
 * as SQL before they run.
 */
export function DetailPanel(props: Props) {
  const { introspection, target, onClose, width, onResize, onResetWidth } = props;
  const t = useT();
  const [tab, setTab] = useState<"detail" | "ddl">("detail");
  const [editing, setEditing] = useState(false);
  const found = useMemo(() => find(introspection, target), [introspection, target]);

  useEffect(() => {
    setTab("detail");
    setEditing(false);
  }, [target]);

  return (
    <PanelShell
      label={target.name}
      width={width}
      onResize={onResize}
      onResetWidth={onResetWidth}
      onClose={onClose}
      header={
        <>
          <span className="detail-kind">{found ? kindLabel(found, t) : ""}</span>
          <strong className="detail-name" title={`${target.schema}.${target.name}`}>
            {target.schema}.{target.name}
          </strong>
          {found && !editing && (
            <div className="tabs" role="tablist">
              {(["detail", "ddl"] as const).map((name) => (
                <button
                  key={name}
                  role="tab"
                  aria-selected={tab === name}
                  className={tab === name ? "selected" : undefined}
                  onClick={() => setTab(name)}
                >
                  {t.detail.tabs[name]}
                </button>
              ))}
            </div>
          )}
          <span className="spacer" />
          {found?.type === "table" && !editing && (
            <button onClick={() => props.onShowErd(found.schema, found.table.name)}>{t.detail.erd}</button>
          )}
          {found && !editing && (
            <button
              onClick={() => {
                setTab("detail");
                setEditing(true);
              }}
            >
              {t.detail.edit}
            </button>
          )}
        </>
      }
    >
        {!found ? (
          <div className="detail-body">
            <p className="muted">{t.detail.notFound}</p>
          </div>
        ) : editing ? (
          <Editor
            {...props}
            found={found}
            onDone={(renamed) => {
              setEditing(false);
              props.onChanged(renamed);
            }}
            onCancel={() => setEditing(false)}
          />
        ) : tab === "ddl" ? (
          <DdlTab found={found} onInsert={props.onInsert} />
        ) : (
          <div className="detail-body">
            {found.type === "table" && (
              <TableDetail
                schema={found.schema}
                table={found.table}
                foreignKeys={introspection.foreignKeys}
                onNavigate={props.onNavigate}
              />
            )}
            {found.type === "view" && <ViewDetail view={found.view} />}
            {found.type === "function" && <FunctionDetail fn={found.fn} />}
          </div>
        )}
    </PanelShell>
  );
}

/** The right-hand panel: resizable from its left edge, closed with ✕ or a click beside it. */
export function PanelShell({
  label,
  width,
  onResize,
  onResetWidth,
  onClose,
  header,
  children,
}: {
  label: string;
  width: number;
  onResize: (width: number) => void;
  onResetWidth: () => void;
  onClose: () => void;
  header: ReactNode;
  children: ReactNode;
}) {
  const t = useT();
  return (
    <div className="panel-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <aside className="side-panel detail-panel" role="dialog" aria-label={label} style={{ width: `min(${width}px, 100%)` }}>
        <Splitter
          orientation="vertical"
          size={width}
          bounds={() => [420, window.innerWidth - 80]}
          grows={-1}
          onResize={onResize}
          onReset={onResetWidth}
          label={t.workspace.resizePanel}
        />
        <header className="side-panel-header">
          {header}
          <button onClick={onClose} title={t.common.close}>
            ✕
          </button>
        </header>
        {children}
      </aside>
    </div>
  );
}

type NewObject = "table" | "view" | "function";

/** Writes a new table, view or function, reviewed as SQL before it runs like an edit. */
export function CreatePanel({
  type,
  schema,
  onCreated,
  onClose,
  width,
  onResize,
  onResetWidth,
  ...base
}: {
  sessionId: number;
  introspection: Introspection;
  type: NewObject;
  schema: string;
  /** After the object was created; null when its name isn't known, as for a function. */
  onCreated: (created: ObjectRef | null) => void;
  onInsert: (sql: string) => void;
  onClose: () => void;
  width: number;
  onResize: (width: number) => void;
  onResetWidth: () => void;
}) {
  const t = useT();
  const editor = { ...base, onDone: onCreated, onCancel: onClose, creating: true };
  return (
    <PanelShell
      label={t.detail.creating[type]}
      width={width}
      onResize={onResize}
      onResetWidth={onResetWidth}
      onClose={onClose}
      header={
        <>
          <span className="detail-kind">{t.detail.creating[type]}</span>
          <strong className="detail-name">{schema}</strong>
          <span className="spacer" />
        </>
      }
    >
      {type === "table" && <TableEditor {...editor} schema={schema} table={NEW_TABLE} />}
      {type === "view" && <ViewEditor {...editor} schema={schema} view={NEW_VIEW} />}
      {type === "function" && <FunctionEditor {...editor} schema={schema} fn={newFunction(schema)} />}
    </PanelShell>
  );
}

/** A starting point: a numbered id that is the primary key. */
const NEW_TABLE: Table = {
  name: "",
  kind: "table",
  columns: [
    {
      name: "id",
      dataType: "bigint",
      nullable: false,
      default: null,
      identity: "always",
      generated: null,
      comment: null,
    },
  ],
  constraints: [{ name: "", kind: "primaryKey", definition: "PRIMARY KEY (id)" }],
  indexes: [],
  partitionKey: null,
  comment: null,
};

const NEW_VIEW: View = { name: "", materialized: false, columns: [], definition: "SELECT\n    \nFROM ", comment: null };

function newFunction(schema: string): SqlFunction {
  return {
    name: "",
    arguments: "",
    kind: "function",
    definition:
      `CREATE OR REPLACE FUNCTION ${quoteIdent(schema)}.new_function(a integer)\n` +
      " RETURNS integer\n LANGUAGE sql\nAS $function$\nSELECT a\n$function$",
  };
}

function find({ database }: Introspection, target: ObjectRef): Found | null {
  const schema = database.schemas.find((s) => s.name === target.schema);
  if (!schema) return null;
  switch (target.type) {
    case "table": {
      const table = schema.tables.find((t) => t.name === target.name);
      return table ? { type: "table", schema: schema.name, table } : null;
    }
    case "view": {
      const view = schema.views.find((v) => v.name === target.name);
      return view ? { type: "view", schema: schema.name, view } : null;
    }
    case "function": {
      const fn = schema.functions.find((f) => f.name === target.name && f.arguments === target.arguments);
      return fn ? { type: "function", schema: schema.name, fn } : null;
    }
  }
}

function kindLabel(found: Found, t: Messages) {
  switch (found.type) {
    case "table":
      return t.detail.kinds.table;
    case "view":
      return found.view.materialized ? t.detail.kinds.materializedView : t.detail.kinds.view;
    case "function":
      return found.fn.kind === "procedure" ? t.detail.kinds.procedure : t.detail.kinds.function;
  }
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="detail-section">
      <h3>{title}</h3>
      {children}
    </section>
  );
}

function Comment({ text }: { text: string | null }) {
  const t = useT();
  return <p className={text ? "detail-comment" : "muted"}>{text ?? t.detail.noComment}</p>;
}

function TableDetail({
  schema,
  table,
  foreignKeys,
  onNavigate,
}: {
  schema: string;
  table: Table;
  foreignKeys: ForeignKey[];
  onNavigate: (target: ObjectRef) => void;
}) {
  const t = useT();
  const primaryKey = primaryKeyColumns(table);
  const outgoing = foreignKeys.filter((fk) => fk.schema === schema && fk.table === table.name);
  const incoming = foreignKeys.filter((fk) => fk.referencedSchema === schema && fk.referencedTable === table.name);
  const keyOf = new Map<string, ForeignKey>();
  for (const fk of outgoing) for (const column of fk.columns) keyOf.set(column, fk);
  const label = (s: string, name: string) => (s === schema ? name : `${s}.${name}`);
  const link = (s: string, name: string) => (
    <button className="link" onClick={() => onNavigate({ type: "table", schema: s, name })}>
      {label(s, name)}
    </button>
  );

  return (
    <>
      <Comment text={table.comment} />
      {table.partitionKey && (
        <p className="muted">
          {t.detail.partitionKey}: <code>{table.partitionKey}</code>
        </p>
      )}

      <Section title={`${t.detail.columns} (${table.columns.length})`}>
        <table className="detail-table">
          <thead>
            <tr>
              <th />
              <th>{t.detail.column}</th>
              <th>{t.detail.type}</th>
              <th>{t.detail.nullable}</th>
              <th>{t.detail.default}</th>
              <th>{t.detail.comment}</th>
            </tr>
          </thead>
          <tbody>
            {table.columns.map((column) => {
              const fk = keyOf.get(column.name);
              return (
                <tr key={column.name}>
                  <td className="key-cell">
                    {primaryKey.includes(column.name) && (
                      <span className="key-badge pk" title={t.detail.primaryKey}>
                        PK
                      </span>
                    )}
                    {fk && (
                      <span
                        className="key-badge fk"
                        title={t.detail.foreignKeyTo(label(fk.referencedSchema, fk.referencedTable))}
                      >
                        FK
                      </span>
                    )}
                  </td>
                  <td className="mono">{column.name}</td>
                  <td className="mono muted">{column.dataType}</td>
                  <td className="mono">{column.nullable ? "" : t.detail.notNull}</td>
                  <td className="mono">{columnDefault(column, t)}</td>
                  <td>{column.comment}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </Section>

      <Section title={t.detail.relationships}>
        {outgoing.length === 0 && incoming.length === 0 && <p className="muted">{t.detail.noRelationships}</p>}
        {outgoing.length > 0 && (
          <>
            <h4>{t.detail.references}</h4>
            <ul className="relationships">
              {outgoing.map((fk) => (
                <li key={fk.name}>
                  <code>({fk.columns.join(", ")})</code> → {link(fk.referencedSchema, fk.referencedTable)}{" "}
                  <code>({fk.referencedColumns.join(", ")})</code>
                  <span className="muted">
                    {fk.name}
                    {actionsText(fk, t)}
                  </span>
                </li>
              ))}
            </ul>
          </>
        )}
        {incoming.length > 0 && (
          <>
            <h4>{t.detail.referencedBy}</h4>
            <ul className="relationships">
              {incoming.map((fk) => (
                <li key={`${fk.schema}.${fk.table}.${fk.name}`}>
                  {link(fk.schema, fk.table)} <code>({fk.columns.join(", ")})</code> →{" "}
                  <code>({fk.referencedColumns.join(", ")})</code>
                  <span className="muted">
                    {fk.name}
                    {actionsText(fk, t)}
                  </span>
                </li>
              ))}
            </ul>
          </>
        )}
      </Section>

      <Section title={`${t.detail.constraints} (${table.constraints.length})`}>
        {table.constraints.length === 0 ? (
          <p className="muted">{t.detail.none}</p>
        ) : (
          <ul className="definitions">
            {table.constraints.map((constraint) => (
              <li key={constraint.name}>
                <span className="mono">{constraint.name}</span>
                <code>{constraint.definition}</code>
              </li>
            ))}
          </ul>
        )}
      </Section>

      <Section title={`${t.detail.indexes} (${table.indexes.length})`}>
        {table.indexes.length === 0 ? (
          <p className="muted">{t.detail.none}</p>
        ) : (
          <ul className="definitions">
            {table.indexes.map((index) => (
              <li key={index.name}>
                <span className="mono">{index.name}</span>
                <code>{index.definition}</code>
              </li>
            ))}
          </ul>
        )}
      </Section>
    </>
  );
}

function ViewDetail({ view }: { view: View }) {
  const t = useT();
  return (
    <>
      <Comment text={view.comment} />
      <Section title={`${t.detail.columns} (${view.columns.length})`}>
        <table className="detail-table">
          <thead>
            <tr>
              <th>{t.detail.column}</th>
              <th>{t.detail.type}</th>
              <th>{t.detail.comment}</th>
            </tr>
          </thead>
          <tbody>
            {view.columns.map((column) => (
              <tr key={column.name}>
                <td className="mono">{column.name}</td>
                <td className="mono muted">{column.dataType}</td>
                <td>{column.comment}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </Section>
      <Section title={t.detail.definition}>
        <pre className="detail-code">{view.definition.trim()}</pre>
      </Section>
    </>
  );
}

function FunctionDetail({ fn }: { fn: SqlFunction }) {
  const t = useT();
  return (
    <>
      <p>
        <span className="muted">{t.detail.arguments}:</span> <code>{fn.arguments || t.detail.none}</code>
      </p>
      <Section title={t.detail.definition}>
        <pre className="detail-code">{fn.definition.trim()}</pre>
      </Section>
    </>
  );
}

function DdlTab({ found, onInsert }: { found: Found; onInsert: (sql: string) => void }) {
  const t = useT();
  const [ddl, setDdl] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    setDdl(null);
    setError(null);
    snapshots
      .createStatement(found.schema, schemaObject(found))
      .then((text) => setDdl(text.trimEnd()))
      .catch(setError);
  }, [found]);

  async function copy() {
    if (!ddl) return;
    await navigator.clipboard.writeText(ddl);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }

  return (
    <>
      <div className="detail-body">
        {error !== null && <p className="error-text">{errorMessage(error, t)}</p>}
        {ddl === null && error === null && <p className="muted">{t.common.loading}</p>}
        {ddl !== null && <pre className="detail-code">{ddl}</pre>}
      </div>
      <footer className="detail-footer">
        <div className="actions">
          <span className="spacer" />
          <button onClick={copy} disabled={!ddl}>
            {copied ? t.detail.copied : t.detail.copy}
          </button>
          <button className="primary" onClick={() => ddl && onInsert(ddl)} disabled={!ddl}>
            {t.detail.insert}
          </button>
        </div>
      </footer>
    </>
  );
}

function schemaObject(found: Found): SchemaObject {
  switch (found.type) {
    case "table":
      return { type: "table", ...found.table };
    case "view":
      return { type: "view", ...found.view };
    case "function":
      return { type: "function", ...found.fn };
  }
}

interface EditorBase {
  sessionId: number;
  introspection: Introspection;
  onInsert: (sql: string) => void;
  /** After the change was applied, with the object's new name when it got one. */
  onDone: (renamed: ObjectRef | null) => void;
  onCancel: () => void;
  /** Writing a new object instead of changing one. */
  creating?: boolean;
}

interface EditorProps extends Props {
  found: Found;
  onDone: (renamed: ObjectRef | null) => void;
  onCancel: () => void;
}

function Editor(props: EditorProps) {
  switch (props.found.type) {
    case "table":
      return <TableEditor {...props} schema={props.found.schema} table={props.found.table} />;
    case "view":
      return <ViewEditor {...props} schema={props.found.schema} view={props.found.view} />;
    case "function":
      return <FunctionEditor {...props} schema={props.found.schema} fn={props.found.fn} />;
  }
}

/**
 * Shows the editor's form, then the statements its changes make, which run in
 * one transaction once the user applies them, or go into the editor.
 */
function EditFlow({
  sessionId,
  build,
  onInsert,
  onApplied,
  onCancel,
  children,
}: {
  sessionId: number;
  /** The edit from the form; throws an Error with a message when the form is incomplete. */
  build: () => ObjectEdit;
  onInsert: (sql: string) => void;
  onApplied: (edit: ObjectEdit) => void;
  onCancel: () => void;
  children: ReactNode;
}) {
  const t = useT();
  const [review, setReview] = useState<{ edit: ObjectEdit; plan: Plan } | null>(null);
  const [allowDestructive, setAllowDestructive] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  async function startReview() {
    setBusy(true);
    setError(null);
    try {
      const edit = build();
      setReview({ edit, plan: await objects.plan(sessionId, edit) });
      setAllowDestructive(false);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  async function apply() {
    if (!review) return;
    setBusy(true);
    setError(null);
    try {
      await objects.apply(
        sessionId,
        review.edit,
        review.plan.steps.map((step) => step.sql),
        allowDestructive,
      );
      onApplied(review.edit);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  const errorText = error !== null && <p className="error-text">{errorMessage(error, t)}</p>;

  if (!review) {
    return (
      <>
        <div className="detail-body detail-editor">{children}</div>
        <footer className="detail-footer">
          {errorText}
          <div className="actions">
            <span className="spacer" />
            <button onClick={onCancel} disabled={busy}>
              {t.detail.editing.cancel}
            </button>
            <button className="primary" onClick={startReview} disabled={busy}>
              {t.detail.editing.review}
            </button>
          </div>
        </footer>
      </>
    );
  }

  const { plan } = review;
  const destructive = plan.steps.filter((step) => step.destructive).length;
  const canApply =
    !busy && plan.steps.length > 0 && plan.blockers.length === 0 && (destructive === 0 || allowDestructive);
  return (
    <>
      <div className="detail-body">
        <p>
          <strong>{t.detail.editing.reviewTitle}</strong>{" "}
          <span className="muted">{t.detail.editing.oneTransaction}</span>
        </p>
        {plan.blockers.length > 0 && (
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
        {plan.steps.length === 0 ? (
          <p className="muted">{t.detail.editing.noChanges}</p>
        ) : (
          <ol className="plan-steps">
            {plan.steps.map((step, i) => (
              <li key={i} className={step.destructive ? "destructive" : undefined}>
                {step.destructive && <span className="step-warning">{t.detail.editing.losesData}</span>}
                <pre>{step.sql}</pre>
              </li>
            ))}
          </ol>
        )}
      </div>
      <footer className="detail-footer">
        {errorText}
        {destructive > 0 && (
          <label className="checkbox destructive-consent">
            <input
              type="checkbox"
              checked={allowDestructive}
              onChange={(e) => setAllowDestructive(e.target.checked)}
              disabled={busy}
            />
            {t.detail.editing.allowDestructive(destructive)}
          </label>
        )}
        <div className="actions">
          <button onClick={() => setReview(null)} disabled={busy}>
            {t.detail.editing.back}
          </button>
          <span className="spacer" />
          <button
            onClick={() => onInsert(plan.steps.map((step) => step.sql).join("\n"))}
            disabled={plan.steps.length === 0}
          >
            {t.detail.insert}
          </button>
          <button className={destructive > 0 ? "primary danger-fill" : "primary"} onClick={apply} disabled={!canApply}>
            {busy ? t.detail.editing.applying : t.detail.editing.apply}
          </button>
        </div>
      </footer>
    </>
  );
}

interface ColumnDraft {
  key: number;
  /** The column as it is in the database; null for a new one. */
  original: Column | null;
  name: string;
  dataType: string;
  nullable: boolean;
  /** Numbered by the database: GENERATED ... AS IDENTITY. */
  identity: Column["identity"];
  default: string;
  comment: string;
  removed: boolean;
}

interface Draft<T> {
  item: T;
  /** Added in this edit, so removing it just takes it out again. */
  added: boolean;
  removed: boolean;
}

const COMMON_TYPES = [
  "bigint",
  "integer",
  "smallint",
  "numeric(12,2)",
  "real",
  "double precision",
  "text",
  "character varying(255)",
  "boolean",
  "date",
  "timestamp with time zone",
  "timestamp without time zone",
  "time without time zone",
  "interval",
  "uuid",
  "jsonb",
  "json",
  "bytea",
  "inet",
  "text[]",
];

function TableEditor({
  sessionId,
  introspection,
  schema,
  table,
  onInsert,
  onDone,
  onCancel,
  creating = false,
}: EditorBase & { schema: string; table: Table }) {
  const t = useT();
  const e = t.detail.editing;
  const [name, setName] = useState(table.name);
  const [comment, setComment] = useState(table.comment ?? "");
  const [nextKey, setNextKey] = useState(table.columns.length);
  const [columns, setColumns] = useState<ColumnDraft[]>(() =>
    table.columns.map((column, key) => ({
      key,
      original: creating ? null : column,
      name: column.name,
      dataType: column.dataType,
      nullable: column.nullable,
      identity: column.identity,
      default: column.default ?? "",
      comment: column.comment ?? "",
      removed: false,
    })),
  );
  const [constraints, setConstraints] = useState<Draft<Constraint>[]>(() =>
    table.constraints.map((item) => ({ item, added: creating, removed: false })),
  );
  const [indexes, setIndexes] = useState<Draft<Index>[]>(() =>
    table.indexes.map((item) => ({ item, added: false, removed: false })),
  );
  const enumTypes = introspection.database.schemas.flatMap((s) =>
    s.enums.map((en) => (s.name === schema ? quoteIdent(en.name) : `${quoteIdent(s.name)}.${quoteIdent(en.name)}`)),
  );

  const setColumn = (key: number, change: Partial<ColumnDraft>) =>
    setColumns((current) => current.map((c) => (c.key === key ? { ...c, ...change } : c)));

  function addColumn() {
    setColumns((current) => [
      ...current,
      {
        key: nextKey,
        original: null,
        name: "",
        dataType: "text",
        nullable: true,
        identity: null,
        default: "",
        comment: "",
        removed: false,
      },
    ]);
    setNextKey(nextKey + 1);
  }

  function toggle<T>(drafts: Draft<T>[], index: number): Draft<T>[] {
    const draft = drafts[index];
    return draft.added
      ? drafts.filter((_, i) => i !== index)
      : drafts.map((d, i) => (i === index ? { ...d, removed: !d.removed } : d));
  }

  function build(): ObjectEdit {
    const kept = columns.filter((c) => !c.removed);
    if (!name.trim() || kept.some((c) => !c.name.trim() || !c.dataType.trim())) throw new Error(e.emptyName);
    const names = kept.map((c) => c.name.trim());
    const duplicate = names.find((n, i) => names.indexOf(n) !== i);
    if (duplicate) throw new Error(e.duplicateColumn(duplicate));
    const tableName = name.trim();
    const edited: Table = {
      ...table,
      name: tableName,
      comment: sameText(comment, table.comment) ? table.comment : comment.trim() || null,
      columns: kept.map((c) => ({
        name: c.name.trim(),
        dataType: c.dataType.trim(),
        // Identity columns are NOT NULL and take no default.
        nullable: c.identity ? false : c.nullable,
        default: c.original?.generated || c.identity ? null : c.default.trim() || null,
        identity: c.identity,
        generated: c.original?.generated ?? null,
        comment: c.original && sameText(c.comment, c.original.comment) ? c.original.comment : c.comment.trim() || null,
      })),
      constraints: constraints
        .filter((d) => !d.removed)
        // The starting primary key is named after the table once it has a name.
        .map((d) => (d.item.name ? d.item : { ...d.item, name: `${tableName}_pkey` })),
      indexes: indexes.filter((d) => !d.removed).map((d) => d.item),
    };
    if (creating) return { kind: "createTable", schema, table: edited };
    return {
      kind: "table",
      schema,
      name: table.name,
      table: edited,
      renamedColumns: kept
        .filter((c) => c.original && c.original.name !== c.name.trim())
        .map((c) => [c.original!.name, c.name.trim()]),
    };
  }

  return (
    <EditFlow
      sessionId={sessionId}
      build={build}
      onInsert={onInsert}
      onCancel={onCancel}
      onApplied={() =>
        onDone(name.trim() !== table.name ? { type: "table", schema, name: name.trim() } : null)
      }
    >
      <div className="field-row">
        <label className="field">
          {e.name}
          <input
            value={name}
            placeholder={t.detail.creating.tableName}
            onChange={(ev) => setName(ev.target.value)}
            autoFocus={creating}
          />
        </label>
        <label className="field grow">
          {t.detail.comment}
          <input value={comment} onChange={(ev) => setComment(ev.target.value)} />
        </label>
      </div>

      <Section title={t.detail.columns}>
        <datalist id="column-types">
          {[...COMMON_TYPES, ...enumTypes].map((type) => (
            <option key={type} value={type} />
          ))}
        </datalist>
        <table className="detail-table editing">
          <thead>
            <tr>
              <th>{t.detail.column}</th>
              <th>{t.detail.type}</th>
              <th>{t.detail.notNull}</th>
              <th title={e.identityTitle}>{e.identity}</th>
              <th>{t.detail.default}</th>
              <th>{t.detail.comment}</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {columns.map((c) => {
              const generated = c.original?.generated ?? null;
              const identity = c.identity;
              return (
                <tr key={c.key} className={c.removed ? "removed" : c.original ? undefined : "added"}>
                  <td>
                    <input
                      value={c.name}
                      placeholder={e.newColumn}
                      onChange={(ev) => setColumn(c.key, { name: ev.target.value })}
                      disabled={c.removed}
                      aria-label={t.detail.column}
                    />
                  </td>
                  <td>
                    <input
                      value={c.dataType}
                      list="column-types"
                      onChange={(ev) => setColumn(c.key, { dataType: ev.target.value })}
                      disabled={c.removed || generated !== null}
                      aria-label={t.detail.type}
                    />
                  </td>
                  <td className="center">
                    <input
                      type="checkbox"
                      checked={!c.nullable}
                      onChange={(ev) => setColumn(c.key, { nullable: !ev.target.checked })}
                      disabled={c.removed || identity !== null}
                      aria-label={t.detail.notNull}
                    />
                  </td>
                  <td className="center">
                    <input
                      type="checkbox"
                      checked={identity !== null}
                      onChange={(ev) =>
                        setColumn(c.key, {
                          identity: ev.target.checked ? (c.original?.identity ?? "always") : null,
                          nullable: ev.target.checked ? false : c.nullable,
                        })
                      }
                      disabled={c.removed || generated !== null}
                      title={e.identityTitle}
                      aria-label={e.identity}
                    />
                  </td>
                  <td>
                    {generated || identity ? (
                      <span className="muted mono">
                        {columnDefault({ ...(c.original ?? NEW_TABLE.columns[0]), identity, generated }, t)}
                      </span>
                    ) : (
                      <input
                        value={c.default}
                        onChange={(ev) => setColumn(c.key, { default: ev.target.value })}
                        disabled={c.removed}
                        aria-label={t.detail.default}
                      />
                    )}
                  </td>
                  <td>
                    <input
                      value={c.comment}
                      onChange={(ev) => setColumn(c.key, { comment: ev.target.value })}
                      disabled={c.removed}
                      aria-label={t.detail.comment}
                    />
                  </td>
                  <td>
                    <button
                      className="icon-button"
                      title={c.removed ? e.restoreColumn : e.removeColumn}
                      aria-label={c.removed ? e.restoreColumn : e.removeColumn}
                      onClick={() =>
                        c.original
                          ? setColumn(c.key, { removed: !c.removed })
                          : setColumns((current) => current.filter((x) => x.key !== c.key))
                      }
                    >
                      {c.removed ? "↺" : "✕"}
                    </button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        <div className="actions">
          <button onClick={addColumn}>{e.addColumn}</button>
        </div>
      </Section>

      <Section title={t.detail.constraints}>
        <DraftList
          drafts={constraints}
          label={(c) => c.name || `${name.trim() || table.name || "…"}_pkey`}
          detail={(c) => c.definition}
          onToggle={(i) => setConstraints((current) => toggle(current, i))}
        />
        <ConstraintForm
          schema={schema}
          table={name.trim() || table.name}
          introspection={introspection}
          onAdd={(constraint) => setConstraints((current) => [...current, { item: constraint, added: true, removed: false }])}
        />
      </Section>

      <Section title={t.detail.indexes}>
        <DraftList
          drafts={indexes}
          label={(i) => i.name}
          detail={(i) => i.definition}
          onToggle={(i) => setIndexes((current) => toggle(current, i))}
        />
        <IndexForm
          schema={schema}
          table={name.trim() || table.name}
          onAdd={(index) => setIndexes((current) => [...current, { item: index, added: true, removed: false }])}
        />
      </Section>
    </EditFlow>
  );
}

function DraftList<T>({
  drafts,
  label,
  detail,
  onToggle,
}: {
  drafts: Draft<T>[];
  label: (item: T) => string;
  detail: (item: T) => string;
  onToggle: (index: number) => void;
}) {
  const t = useT();
  if (drafts.length === 0) return <p className="muted">{t.detail.none}</p>;
  return (
    <ul className="definitions editing">
      {drafts.map((draft, i) => (
        <li key={`${label(draft.item)}-${i}`} className={draft.removed ? "removed" : draft.added ? "added" : undefined}>
          <span className="mono">{label(draft.item)}</span>
          <code>{detail(draft.item)}</code>
          <button onClick={() => onToggle(i)}>{draft.removed ? t.detail.editing.restore : t.detail.editing.remove}</button>
        </li>
      ))}
    </ul>
  );
}

type NewConstraintKind = "primaryKey" | "unique" | "check" | "foreignKey";
const ACTIONS = ["NO ACTION", "RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT"];

function ConstraintForm({
  schema,
  table,
  introspection,
  onAdd,
}: {
  schema: string;
  table: string;
  introspection: Introspection;
  onAdd: (constraint: Constraint) => void;
}) {
  const t = useT();
  const e = t.detail.editing;
  const [open, setOpen] = useState(false);
  const [kind, setKind] = useState<NewConstraintKind>("foreignKey");
  const [name, setName] = useState("");
  const [columns, setColumns] = useState("");
  const [check, setCheck] = useState("");
  const tables = introspection.database.schemas.flatMap((s) => s.tables.map((tb) => ({ schema: s.name, table: tb })));
  // Start with a table in the same schema.
  const [reference, setReference] = useState(() => Math.max(0, tables.findIndex((tb) => tb.schema === schema)));
  const [referencedColumns, setReferencedColumns] = useState("");
  const [onDelete, setOnDelete] = useState("NO ACTION");
  const [onUpdate, setOnUpdate] = useState("NO ACTION");

  const columnList = splitNames(columns);
  const defaultName = {
    primaryKey: `${table}_pkey`,
    unique: `${table}_${columnList.join("_")}_key`,
    check: `${table}_check`,
    foreignKey: `${table}_${columnList.join("_")}_fkey`,
  }[kind];
  const referenced = tables[reference];
  const referencedList = referencedColumns.trim()
    ? splitNames(referencedColumns)
    : referenced
      ? primaryKeyColumns(referenced.table)
      : [];

  const ready =
    kind === "check" ? check.trim() !== "" : columnList.length > 0 && (kind !== "foreignKey" || (!!referenced && referencedList.length > 0));

  function add() {
    const cols = columnList.map(quoteIdent).join(", ");
    const definition = {
      primaryKey: () => `PRIMARY KEY (${cols})`,
      unique: () => `UNIQUE (${cols})`,
      check: () => `CHECK (${check.trim()})`,
      foreignKey: () =>
        `FOREIGN KEY (${cols}) REFERENCES ${quoteIdent(referenced.schema)}.${quoteIdent(referenced.table.name)}` +
        `(${referencedList.map(quoteIdent).join(", ")})` +
        (onUpdate !== "NO ACTION" ? ` ON UPDATE ${onUpdate}` : "") +
        (onDelete !== "NO ACTION" ? ` ON DELETE ${onDelete}` : ""),
    }[kind]();
    onAdd({ name: name.trim() || defaultName, kind, definition });
    setOpen(false);
    setName("");
    setColumns("");
    setCheck("");
    setReferencedColumns("");
  }

  if (!open) {
    return (
      <div className="actions">
        <button onClick={() => setOpen(true)}>{e.addConstraint}</button>
      </div>
    );
  }
  return (
    <div className="add-form">
      <div className="field-row">
        <label className="field">
          {t.detail.type}
          <select value={kind} onChange={(ev) => setKind(ev.target.value as NewConstraintKind)}>
            {(Object.keys(e.constraintKinds) as NewConstraintKind[]).map((k) => (
              <option key={k} value={k}>
                {e.constraintKinds[k]}
              </option>
            ))}
          </select>
        </label>
        <label className="field grow">
          {e.constraintName}
          <input value={name} placeholder={defaultName} onChange={(ev) => setName(ev.target.value)} />
        </label>
      </div>
      {kind === "check" ? (
        <label className="field">
          {e.checkExpression}
          <input value={check} onChange={(ev) => setCheck(ev.target.value)} className="mono" />
        </label>
      ) : (
        <label className="field">
          {e.constraintColumns}
          <input value={columns} onChange={(ev) => setColumns(ev.target.value)} className="mono" />
        </label>
      )}
      {kind === "foreignKey" && (
        <>
          <div className="field-row">
            <label className="field grow">
              {e.referencedTable}
              <select value={reference} onChange={(ev) => setReference(Number(ev.target.value))}>
                {tables.map((tb, i) => (
                  <option key={`${tb.schema}.${tb.table.name}`} value={i}>
                    {tb.schema}.{tb.table.name}
                  </option>
                ))}
              </select>
            </label>
            <label className="field grow">
              {e.referencedColumns}
              <input
                value={referencedColumns}
                placeholder={referenced ? primaryKeyColumns(referenced.table).join(", ") : ""}
                onChange={(ev) => setReferencedColumns(ev.target.value)}
                className="mono"
              />
            </label>
          </div>
          <div className="field-row">
            <label className="field">
              {t.detail.onDelete}
              <select value={onDelete} onChange={(ev) => setOnDelete(ev.target.value)}>
                {ACTIONS.map((a) => (
                  <option key={a}>{a}</option>
                ))}
              </select>
            </label>
            <label className="field">
              {t.detail.onUpdate}
              <select value={onUpdate} onChange={(ev) => setOnUpdate(ev.target.value)}>
                {ACTIONS.map((a) => (
                  <option key={a}>{a}</option>
                ))}
              </select>
            </label>
          </div>
        </>
      )}
      <div className="actions">
        <span className="spacer" />
        <button onClick={() => setOpen(false)}>{e.cancel}</button>
        <button className="primary" onClick={add} disabled={!ready}>
          {e.add}
        </button>
      </div>
    </div>
  );
}

function IndexForm({ schema, table, onAdd }: { schema: string; table: string; onAdd: (index: Index) => void }) {
  const t = useT();
  const e = t.detail.editing;
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [columns, setColumns] = useState("");
  const [unique, setUnique] = useState(false);
  const parts = columns
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean);
  // Plain names are quoted; anything else, such as lower(email), is an expression.
  const keys = parts.map((part) => (/^[\p{L}_][\p{L}\p{N}_$]*$/u.test(part) ? quoteIdent(part) : part));
  const defaultName = `${table}_${parts.map((p) => p.replace(/\W+/g, "_")).join("_")}_idx`;

  function add() {
    const indexName = name.trim() || defaultName;
    onAdd({
      name: indexName,
      definition: `CREATE ${unique ? "UNIQUE " : ""}INDEX ${quoteIdent(indexName)} ON ${quoteIdent(schema)}.${quoteIdent(table)} USING btree (${keys.join(", ")})`,
    });
    setOpen(false);
    setName("");
    setColumns("");
    setUnique(false);
  }

  if (!open) {
    return (
      <div className="actions">
        <button onClick={() => setOpen(true)}>{e.addIndex}</button>
      </div>
    );
  }
  return (
    <div className="add-form">
      <div className="field-row">
        <label className="field grow">
          {e.indexName}
          <input value={name} placeholder={defaultName} onChange={(ev) => setName(ev.target.value)} />
        </label>
        <label className="checkbox">
          <input type="checkbox" checked={unique} onChange={(ev) => setUnique(ev.target.checked)} />
          {e.unique}
        </label>
      </div>
      <label className="field">
        {e.indexColumns}
        <input value={columns} onChange={(ev) => setColumns(ev.target.value)} className="mono" />
      </label>
      <div className="actions">
        <span className="spacer" />
        <button onClick={() => setOpen(false)}>{e.cancel}</button>
        <button className="primary" onClick={add} disabled={parts.length === 0}>
          {e.add}
        </button>
      </div>
    </div>
  );
}

function ViewEditor({
  sessionId,
  schema,
  view,
  onInsert,
  onDone,
  onCancel,
  creating = false,
}: EditorBase & { schema: string; view: View }) {
  const t = useT();
  const [name, setName] = useState(view.name);
  const [materialized, setMaterialized] = useState(view.materialized);
  const [definition, setDefinition] = useState(view.definition.trim());
  const [comment, setComment] = useState(view.comment ?? "");
  return (
    <EditFlow
      sessionId={sessionId}
      build={() => {
        const edited = {
          ...view,
          name: name.trim(),
          materialized,
          definition,
          comment: sameText(comment, view.comment) ? view.comment : comment.trim() || null,
        };
        if (!creating) return { kind: "view", schema, name: view.name, view: edited };
        if (!edited.name) throw new Error(t.detail.editing.emptyName);
        return { kind: "createView", schema, view: edited };
      }}
      onInsert={onInsert}
      onCancel={onCancel}
      onApplied={() => onDone(creating ? { type: "view", schema, name: name.trim() } : null)}
    >
      {creating && (
        <div className="field-row">
          <label className="field grow">
            {t.detail.editing.name}
            <input
              value={name}
              placeholder={t.detail.creating.viewName}
              onChange={(ev) => setName(ev.target.value)}
              autoFocus
            />
          </label>
          <label className="checkbox">
            <input type="checkbox" checked={materialized} onChange={(ev) => setMaterialized(ev.target.checked)} />
            {t.detail.creating.materialized}
          </label>
        </div>
      )}
      <label className="field">
        {t.detail.comment}
        <input value={comment} onChange={(ev) => setComment(ev.target.value)} />
      </label>
      <label className="field grow">
        {t.detail.editing.viewQuery}
        <textarea
          className="code-input"
          value={definition}
          onChange={(ev) => setDefinition(ev.target.value)}
          spellCheck={false}
          rows={14}
        />
      </label>
    </EditFlow>
  );
}

function FunctionEditor({
  sessionId,
  schema,
  fn,
  onInsert,
  onDone,
  onCancel,
  creating = false,
}: EditorBase & { schema: string; fn: SqlFunction }) {
  const t = useT();
  const [definition, setDefinition] = useState(fn.definition.trim());
  return (
    <EditFlow
      sessionId={sessionId}
      build={() =>
        creating
          ? { kind: "createFunction", schema, definition }
          : { kind: "function", schema, name: fn.name, arguments: fn.arguments, definition }
      }
      onInsert={onInsert}
      onCancel={onCancel}
      onApplied={() => onDone(null)}
    >
      <label className="field grow">
        {t.detail.editing.functionDefinition}
        <textarea
          className="code-input"
          value={definition}
          onChange={(ev) => setDefinition(ev.target.value)}
          spellCheck={false}
          rows={20}
        />
      </label>
    </EditFlow>
  );
}

/** Whether text typed in a form says the same as the stored value, ignoring surrounding space. */
function sameText(typed: string, stored: string | null) {
  return typed.trim() === (stored ?? "").trim();
}

function columnDefault(column: Column, t: Messages) {
  if (column.generated) return t.detail.generated(column.generated.expression);
  if (column.identity) return t.detail.identity(column.identity === "always" ? "ALWAYS" : "BY DEFAULT");
  return column.default ?? "";
}

function actionsText(fk: ForeignKey, t: Messages) {
  const parts = [
    fk.onDelete !== "NO ACTION" && `${t.detail.onDelete} ${fk.onDelete}`,
    fk.onUpdate !== "NO ACTION" && `${t.detail.onUpdate} ${fk.onUpdate}`,
  ].filter(Boolean);
  return parts.length ? ` · ${parts.join(" · ")}` : "";
}
