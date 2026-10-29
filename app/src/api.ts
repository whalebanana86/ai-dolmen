// Typed wrappers around the Tauri commands in src-tauri/src/lib.rs.
// The shapes mirror dolmen-core and are pinned by crates/dolmen-core/tests/wire_format.rs.
import { Channel, invoke } from "@tauri-apps/api/core";
import type { Messages } from "./i18n/en";

export type SslMode = "disable" | "prefer" | "require" | "verify-full";

export interface ConnectConfig {
  host: string;
  port: number;
  user: string;
  password: string | null;
  database: string;
  sslMode: SslMode;
}

export interface SessionInfo {
  id: number;
  serverVersion: string;
  database: string;
  /** The saved connection it came from, if any. */
  savedId: string | null;
  /** The saved connection's name, or user@host:port/database. */
  label: string;
  /** user@host:port/database, to say which server something will change. */
  address: string;
  environment: Environment | null;
  /** Whether its transactions are read-only for now. */
  readOnly: boolean;
}

/** What a database is for; production gets stricter defaults. */
export type Environment = "development" | "staging" | "production";
export const ENVIRONMENTS: Environment[] = ["development", "staging", "production"];

/** What running a statement could destroy. */
export type Risk =
  | { kind: "everyRow"; verb: string; table: string }
  | { kind: "drop"; object: string; table: string | null }
  | { kind: "truncate"; table: string }
  | { kind: "dropColumn"; table: string; column: string };

export interface RowEstimate {
  rows: number;
  /** Counting stopped early: there are at least `rows`. */
  atLeast: boolean;
}

/** A step of a plan, as EXPLAIN (FORMAT JSON) gives it. */
export interface PlanNode {
  "Node Type": string;
  Plans?: PlanNode[];
  "Relation Name"?: string;
  Schema?: string;
  Alias?: string;
  "Index Name"?: string;
  "Join Type"?: string;
  Strategy?: string;
  "CTE Name"?: string;
  "Function Name"?: string;
  "Subplan Name"?: string;
  [key: string]: unknown;
}

export type ExplainOutput = { Plan: PlanNode; "Planning Time"?: number; "Execution Time"?: number }[];

export interface Assessment {
  risk: Risk;
  /** About how many rows it touches, when that can be told. */
  estimate: RowEstimate | null;
}

export type StatementResult =
  | {
      kind: "rows";
      columns: string[];
      /** Values in PostgreSQL's text format; null is SQL NULL. */
      rows: (string | null)[][];
      /** Rows the statement returned, which can exceed rows.length. */
      totalRows: number;
      /** Where the rows come from, to change them; null when that can't be told. */
      source: RowSource | null;
    }
  | { kind: "command"; rowsAffected: number };

/** Where a result's rows come from, and whether they can be changed. */
export type RowSource =
  | {
      kind: "table";
      schema: string;
      table: string;
      /** For each result column, the table column it shows; null for a computed value or a repeat. */
      columns: (SourceColumn | null)[];
      /** The result columns that hold the primary key, in key order. */
      key: number[];
    }
  | { kind: "readOnly"; reason: ReadOnlyReason };

export type ReadOnlyReason = "noTable" | "view" | "severalTables" | "noPrimaryKey" | "keyNotSelected";

/** A table column a result shows. */
export interface SourceColumn {
  name: string;
  dataType: string;
  nullable: boolean;
  /** Computed from other columns, so it takes no value. */
  generated: boolean;
  /** Only the database gives it values (GENERATED ALWAYS AS IDENTITY). */
  identityAlways: boolean;
  /** A new row gets a value when it's left out: a default, a sequence or an identity. */
  hasDefault: boolean;
}

/** A column's value in PostgreSQL's text format; null is SQL NULL. */
export interface CellValue {
  column: string;
  value: string | null;
}

export type RowChange =
  /** Gives cells of the row with `key` new values, if they still hold the old ones. */
  | { kind: "update"; key: CellValue[]; cells: { column: string; old: string | null; new: string | null }[] }
  /** Adds a row; columns left out get their defaults. */
  | { kind: "insert"; cells: CellValue[] }
  | { kind: "delete"; key: CellValue[] };

export interface RowChanges {
  schema: string;
  table: string;
  /** Made in this order. */
  changes: RowChange[];
  /** The table columns whose values come back for each updated or added row. */
  returning: string[];
}

export type SaveResult =
  /** `rows` has, for each change, the row's `returning` values now, or null for a delete. */
  | { kind: "saved"; committed: boolean; rows: ((string | null)[] | null)[] }
  /** Change `change` found its row changed or gone since it was read; nothing was saved. */
  | { kind: "stale"; change: number }
  /** Change `change` failed; nothing was saved. */
  | { kind: "failed"; change: number; error: AppError };

// The database structure, mirroring dolmen_core::schema. Lists are sorted by name.

export interface Column {
  name: string;
  /** The type as PostgreSQL writes it, e.g. "character varying(255)". */
  dataType: string;
  nullable: boolean;
  default: string | null;
  identity: "always" | "byDefault" | null;
  generated: { expression: string; stored: boolean } | null;
  comment: string | null;
}

export type ConstraintKind = "primaryKey" | "unique" | "check" | "foreignKey" | "exclusion";

export interface Constraint {
  name: string;
  kind: ConstraintKind;
  /** e.g. "PRIMARY KEY (id)" */
  definition: string;
}

export interface Index {
  name: string;
  /** The full CREATE INDEX statement. */
  definition: string;
}

export interface Table {
  name: string;
  kind: "table" | "partitioned" | "foreign";
  columns: Column[];
  constraints: Constraint[];
  /** Indexes that don't back a primary key, unique or exclusion constraint. */
  indexes: Index[];
  /** The PARTITION BY clause of a partitioned table, e.g. "RANGE (created_at)". */
  partitionKey: string | null;
  comment: string | null;
  /** For a partition: the table it belongs to and its bounds. Its columns come from there. */
  partitionOf?: { schema: string; table: string; bound: string } | null;
  /** Privileges held by roles other than the owner. */
  grants?: Grant[];
  /** Triggers, besides the ones PostgreSQL makes for foreign keys. */
  triggers?: Trigger[];
  /** Whether row-level security is on, so only rows the policies allow are seen. */
  rowSecurity?: boolean;
  /** Whether row-level security applies to the table's owner too. */
  forceRowSecurity?: boolean;
  policies?: Policy[];
}

export interface Trigger {
  name: string;
  /** The full CREATE TRIGGER statement. */
  definition: string;
  /** Left out when it fires the usual way ("origin"). */
  firing?: "origin" | "replica" | "always" | "disabled";
}

/** A row-level security policy. */
export interface Policy {
  name: string;
  command: "ALL" | "SELECT" | "INSERT" | "UPDATE" | "DELETE";
  /** Permissive policies are alternatives; restrictive ones must all pass. */
  permissive: boolean;
  /** Roles it applies to, or "PUBLIC". */
  roles: string[];
  using: string | null;
  withCheck: string | null;
}

/** Privileges a role (or PUBLIC) holds on a table or view, besides its owner's. */
export interface Grant {
  grantee: string;
  privileges: string[];
  /** Whether the grantee may grant them on. */
  grantable: boolean;
}

export interface View {
  name: string;
  materialized: boolean;
  columns: Column[];
  definition: string;
  comment: string | null;
  /** A materialized view's indexes. */
  indexes?: Index[];
  grants?: Grant[];
  triggers?: Trigger[];
}

export interface SqlFunction {
  name: string;
  /** Identifies the function among overloads, e.g. "a integer, b text". */
  arguments: string;
  kind: "function" | "procedure";
  definition: string;
}

export interface Schema {
  name: string;
  tables: Table[];
  views: View[];
  functions: SqlFunction[];
  /** Sequences not owned by a serial or identity column. */
  sequences: { name: string }[];
  enums: { name: string; labels: string[] }[];
}

export interface Database {
  schemas: Schema[];
  extensions: { name: string; schema: string }[];
}

/** Any schema object, tagged with its type, e.g. to ask for its DDL. */
export type SchemaObject =
  | ({ type: "table" } & Table)
  | ({ type: "view" } & View)
  | ({ type: "function" } & SqlFunction)
  | ({ type: "sequence" } & Schema["sequences"][number])
  | ({ type: "enum" } & Schema["enums"][number]);

export interface Introspection {
  /** The schema unqualified names resolve to first, usually "public". */
  defaultSchema: string | null;
  database: Database;
  /** The relationships between tables. */
  foreignKeys: ForeignKey[];
}

/** A foreign key with its columns spelled out, in key order. */
export interface ForeignKey {
  name: string;
  schema: string;
  table: string;
  columns: string[];
  referencedSchema: string;
  referencedTable: string;
  referencedColumns: string[];
  /** "NO ACTION", "RESTRICT", "CASCADE", "SET NULL" or "SET DEFAULT". */
  onUpdate: string;
  onDelete: string;
}

/** A table, view or function, as the detail view and the ERD name it. */
export type ObjectRef =
  | { type: "table"; schema: string; name: string }
  | { type: "view"; schema: string; name: string }
  | { type: "function"; schema: string; name: string; arguments: string };

/** A change made in the detail view; the backend plans the statements. */
export type ObjectEdit =
  | {
      kind: "table";
      schema: string;
      /** The table's current name; `table.name` may be a new one. */
      name: string;
      table: Table;
      /** [old name, new name] of renamed columns, which keep their data. */
      renamedColumns: [string, string][];
    }
  | { kind: "view"; schema: string; name: string; view: View }
  | { kind: "function"; schema: string; name: string; arguments: string; definition: string }
  | { kind: "createTable"; schema: string; table: Table }
  | { kind: "createView"; schema: string; view: View }
  /** A CREATE FUNCTION or CREATE PROCEDURE statement. */
  | { kind: "createFunction"; schema: string; definition: string };

export type AppError =
  | {
      kind: "database";
      message: string;
      code: string;
      detail: string | null;
      hint: string | null;
      /** 1-based character position in the submitted SQL. */
      position: number | null;
    }
  | { kind: "connection"; message: string }
  | { kind: "closed" }
  | { kind: "storage"; message: string }
  | { kind: "refused"; reason: Refusal; message: string }
  | { kind: "ai"; reason: AiFailure; message: string };

/** Why the backend refused an action; the UI words each in the user's language. */
export type Refusal =
  | "notAllowed"
  | "planChanged"
  | "planBlocked"
  | "destructiveNotAllowed"
  | "noModel"
  | "badSnapshotPath"
  | "uncommittedChanges"
  | "readOnly"
  | "noColumns"
  | "developmentBuild"
  | "tableChanged"
  | "rowChanged";

/** Why asking the AI failed. */
export type AiFailure =
  | "noApiKey"
  | "badApiKey"
  | "rateLimited"
  | "unavailable"
  | "declined"
  | "network"
  | "cancelled"
  | "other";

/** The models to choose from, the default first. */
export const AI_MODELS = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"] as const;
export type AiModel = (typeof AI_MODELS)[number];

export type AiTask =
  | {
      kind: "generate";
      request: string;
      /** The selected text or the statement at the cursor, which the request may be about. */
      context: string | null;
    }
  | { kind: "explain"; sql: string }
  | { kind: "fix"; sql: string; error: string };

export interface AiQuestion {
  /** Chosen by the UI, to stop the answer with `ai.cancel`. */
  id: number;
  sessionId: number;
  model: AiModel;
  /** The language to answer in, e.g. "Korean". */
  language: string;
  task: AiTask;
}

export interface AiAnswer {
  /** The model that wrote it; another one when the API fell back. */
  model: string;
  /** "end_turn", or "max_tokens" when the answer was cut off. */
  stopReason: string | null;
}

export function appVersion() {
  return invoke<string>("app_version");
}

/** A release newer than the running app. */
export interface AvailableUpdate {
  version: string;
  notes: string | null;
}

export interface UpdateDownload {
  bytes: number;
  total: number | null;
}

export const updates = {
  /** The newest release from the project's GitHub releases, or null when this version is the latest. */
  check: () => invoke<AvailableUpdate | null>("update_check"),
  /** Downloads and installs what `check` found, then restarts the app. */
  install: (onProgress: (download: UpdateDownload) => void) => {
    const progress = new Channel<UpdateDownload>();
    progress.onmessage = onProgress;
    return invoke<void>("update_install", { progress });
  },
};

export const pg = {
  /** Without a password in `config`, the stored password of `savedId` is used. */
  connect: (config: ConnectConfig, savedId: string | null = null) =>
    invoke<SessionInfo>("pg_connect", { config, savedId }),
  execute: (sessionId: number, sql: string) =>
    invoke<StatementResult[]>("pg_execute", { sessionId, sql }),
  introspect: (sessionId: number) => invoke<Introspection>("pg_introspect", { sessionId }),
  cancel: (sessionId: number) => invoke<void>("pg_cancel", { sessionId }),
  disconnect: (sessionId: number) => invoke<void>("pg_disconnect", { sessionId }),
  /** Another session to the same database, for an editor tab of its own. */
  fork: (sessionId: number) => invoke<SessionInfo>("pg_fork", { sessionId }),
  /** Makes the session's unqualified names resolve in `schema` alone; returns where they now do. */
  setSchema: (sessionId: number, schema: string) => invoke<string | null>("pg_set_schema", { sessionId, schema }),
  /** The schema the session's unqualified names resolve in first. */
  currentSchema: (sessionId: number) => invoke<string | null>("pg_current_schema", { sessionId }),
  /** Makes the session and every tab's session of its connection read-only, or lets them write. */
  setReadOnly: (sessionId: number, readOnly: boolean) => invoke<void>("pg_set_read_only", { sessionId, readOnly }),
  /** The statement's plan; with `analyze`, measured by running it and rolling back. */
  explain: (sessionId: number, statement: string, analyze: boolean) =>
    invoke<ExplainOutput>("pg_explain", { sessionId, statement, analyze }),
  /** What each statement could destroy, with about how many rows; null for a harmless one. */
  assess: (sessionId: number, statements: string[]) =>
    invoke<(Assessment | null)[]>("pg_assess", { sessionId, statements }),
  /** Saves changes to a result's rows, all or none; in an open transaction they join it. */
  saveRows: (sessionId: number, changes: RowChanges) => invoke<SaveResult>("rows_save", { sessionId, changes }),
  /** The statements saving `changes` makes, with their values written in. */
  previewRows: (changes: RowChanges) => invoke<string[]>("rows_preview", { changes }),
};

/** A connection the user saved. Its password, if any, stays in the OS keychain. */
export interface SavedConnection {
  /** Empty for a connection that has not been saved yet. */
  id: string;
  name: string;
  host: string;
  port: number;
  user: string;
  database: string;
  sslMode: SslMode;
  hasPassword: boolean;
  /** Where this database's schema snapshot is written, usually inside a Git repository. */
  snapshotDir: string | null;
  /** A repository Dolmen clones and pushes for the snapshot; used instead of snapshotDir. */
  snapshotRemote: SnapshotRemote | null;
  /** Whether statements run on this connection go into the query history. */
  recordHistory: boolean;
  /** Whether snapshot plans may change this database; unset, only a server on this machine that isn't production. */
  allowSnapshotApply: boolean | null;
  environment: Environment | null;
  /** Whether sessions start read-only; unset, only production's do. */
  readOnly: boolean | null;
}

export interface SnapshotRemote {
  url: string;
  /** The snapshot folder inside the repository, e.g. "db/schema". */
  path: string;
}

export type PasswordChange = { kind: "keep" } | { kind: "set"; password: string } | { kind: "forget" };

export const connections = {
  list: () => invoke<SavedConnection[]>("connections_list"),
  save: (connection: SavedConnection, password: PasswordChange) =>
    invoke<SavedConnection>("connections_save", { connection, password }),
  delete: (id: string) => invoke<void>("connections_delete", { id }),
};

export interface FileChange {
  /** Path inside the snapshot folder, e.g. "public/tables/users.sql". */
  path: string;
  kind: "added" | "modified" | "removed";
  /** The change as a unified diff. */
  diff: string;
}

export interface Repository {
  root: string;
  /** The checked-out branch; null on a detached HEAD. */
  branch: string | null;
  /** The branch's upstream, e.g. "origin/main". */
  upstream: string | null;
  /** Commits the upstream doesn't have yet, as of the last fetch. */
  ahead: number;
  behind: number;
  remotes: string[];
  /** Local branches, then remote branches with no local branch of the same name. */
  branches: string[];
}

export interface Commit {
  hash: string;
  parents: string[];
  author: string;
  /** ISO 8601 author date. */
  date: string;
  subject: string;
  /** Branches and tags at the commit, e.g. "main", "origin/main", "tag: v1". */
  refs: string[];
  /** Whether this is the checked-out commit. */
  head: boolean;
  /** Whether the commit changed the snapshot folder. */
  changesSnapshot: boolean;
}

export interface SnapshotStatus {
  /** The Git repository holding the folder, if any. */
  repository: Repository | null;
  /** What writing the snapshot now would add, change or remove. */
  changes: FileChange[];
  /** Whether the folder already holds a snapshot. */
  initialized: boolean;
}

/** Treats a drop and an add as a rename, which keeps the data. */
export type Rename =
  | { kind: "table"; schema: string; from: string; to: string }
  | { kind: "column"; schema: string; table: string; from: string; to: string };

/** A change the plan can't make. */
export type Blocker =
  | { kind: "enumValuesRemoved"; name: string }
  | { kind: "tableKindChanged"; name: string }
  | { kind: "generatedColumnChanged"; name: string }
  | { kind: "renameNoLongerApplies"; from: string; to: string }
  | { kind: "invalidDefinition"; name: string };

export interface Plan {
  steps: { sql: string; destructive: boolean }[];
  /** Changes Dolmen can't make; a plan with blockers can't be applied. */
  blockers: Blocker[];
  /** Drops paired with adds that look like renames. */
  renameCandidates: Rename[];
}

export interface ApplyPreview {
  plan: Plan;
  /** Whether the connection lets snapshot plans change its database. */
  allowed: boolean;
}

export const snapshots = {
  /**
   * Clones the repository into Dolmen's own folder, or brings that clone up to
   * date (fast-forward only), and returns the snapshot folder inside it.
   */
  /** SSH addresses for an HTTPS repository URL, from the aliases in ~/.ssh/config. */
  sshAlternatives: (url: string) => invoke<string[]>("snapshot_ssh_alternatives", { url }),
  checkout: (url: string, path: string) =>
    invoke<{ dir: string; clone: string; updated: boolean; syncError: string | null }>("snapshot_checkout", {
      url,
      path,
    }),
  status: (sessionId: number, dir: string) =>
    invoke<SnapshotStatus>("snapshot_status", { sessionId, dir }),
  /** Writes the snapshot into `dir`, commits it when a message is given, and pushes when asked. */
  save: (sessionId: number, dir: string, commitMessage: string | null, push: boolean) =>
    invoke<{ commit: string | null; pushed: boolean }>("snapshot_save", {
      sessionId,
      dir,
      commitMessage,
      push,
    }),
  push: (dir: string) => invoke<void>("snapshot_push", { dir }),
  /** Creates a branch at the current commit and checks it out. */
  createBranch: (dir: string, name: string) => invoke<void>("snapshot_create_branch", { dir, name }),
  /** Checks out a branch; refused while the snapshot folder has uncommitted changes. */
  switchBranch: (dir: string, name: string) => invoke<void>("snapshot_switch_branch", { dir, name }),
  /** Recent commits of the repository holding `dir`, on all branches and tags. */
  history: (dir: string) => invoke<Commit[]>("snapshot_history", { dir }),
  /** What would change in the database to make it match the snapshot at `rev`. */
  compare: (sessionId: number, dir: string, rev: string) =>
    invoke<FileChange[]>("snapshot_compare", { sessionId, dir, rev }),
  /** The plan that makes the database match the snapshot at `rev`, or the folder's files when null. */
  plan: (sessionId: number, dir: string, rev: string | null, renames: Rename[]) =>
    invoke<ApplyPreview>("snapshot_plan", { sessionId, dir, rev, renames }),
  /** Applies the reviewed plan, which the backend makes again and checks against `reviewed`. */
  apply: (
    sessionId: number,
    dir: string,
    rev: string | null,
    renames: Rename[],
    reviewed: string[],
    allowDestructive: boolean,
  ) => invoke<void>("snapshot_apply", { sessionId, dir, rev, renames, reviewed, allowDestructive }),
  /** The CREATE statement for one object, as written in snapshots. */
  createStatement: (schema: string, object: SchemaObject) =>
    invoke<string>("ddl_create_statement", { schema, object }),
};

/** Whether a host is this machine, as the backend judges it: localhost, a loopback address or a socket. */
export function isLocalHost(host: string) {
  const name = host.trim().toLowerCase();
  if (name === "localhost" || name.startsWith("/")) return true;
  const address = name.replace(/^\[|\]$/g, "");
  return address === "::1" || /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(address);
}

export const objects = {
  /** The statements that make the object match `edit`, to review. */
  plan: (sessionId: number, edit: ObjectEdit) => invoke<Plan>("object_plan", { sessionId, edit }),
  /** Applies the reviewed statements in one transaction; refused if the plan changed since. */
  apply: (sessionId: number, edit: ObjectEdit, reviewed: string[], allowDestructive: boolean) =>
    invoke<void>("object_apply", { sessionId, edit, reviewed, allowDestructive }),
};

/** A statement run, or a reviewed plan applied, as the query history keeps it. */
export interface HistoryRun {
  id: number;
  connectionId: string | null;
  /** The connection's name, or user@host:port/database. */
  connection: string;
  /** With passwords and other secrets masked, unless that is turned off. */
  sql: string;
  /** Milliseconds since the Unix epoch. */
  startedAt: number;
  durationMs: number;
  status: "ok" | "error" | "cancelled";
  /** Rows returned or affected. */
  rows: number | null;
  error: string | null;
  source: "editor" | "detail" | "snapshot" | "import" | "grid";
  starred: boolean;
}

export interface HistoryFilter {
  /** Only this saved connection's runs; all connections when unset. */
  connectionId?: string | null;
  /** Only runs of the connection with this label, for one that isn't saved. */
  connection?: string | null;
  /** Runs whose SQL contains this text, ignoring case. */
  text?: string | null;
  status?: HistoryRun["status"] | null;
  starredOnly?: boolean;
  /** Runs older than this id, to page through the list. */
  before?: number | null;
  limit?: number;
}

export const history = {
  /** Newest first. */
  list: (filter: HistoryFilter) => invoke<HistoryRun[]>("history_list", { filter }),
  star: (id: number, starred: boolean) => invoke<void>("history_star", { id, starred }),
  delete: (ids: number[]) => invoke<void>("history_delete", { ids }),
  /** Deletes unstarred runs of one saved connection, or of all when null. */
  clear: (connectionId: string | null) => invoke<void>("history_clear", { connectionId }),
  settings: () => invoke<{ redact: boolean }>("history_settings"),
  setSettings: (settings: { redact: boolean }) => invoke<void>("history_set_settings", { settings }),
  /** The editor text last saved for a connection. */
  loadEditor: (key: string) => invoke<string | null>("editor_load", { key }),
  saveEditor: (key: string, content: string) => invoke<void>("editor_save", { key, content }),
};

export const ai = {
  /** Whether an API key is saved in the keychain, or set in ANTHROPIC_API_KEY. */
  keyStatus: () => invoke<{ saved: boolean; fromEnvironment: boolean }>("ai_key_status"),
  /** Saves the API key in the OS keychain. It never comes back to the UI. */
  setKey: (key: string) => invoke<void>("ai_key_set", { key }),
  forgetKey: () => invoke<void>("ai_key_forget"),
  /** Asks about a task, calling `onText` with each piece of the answer as it arrives. */
  ask: (question: AiQuestion, onText: (text: string) => void) => {
    const channel = new Channel<string>();
    channel.onmessage = onText;
    return invoke<AiAnswer>("ai_ask", { question, onText: channel });
  },
  /** Stops the answer to question `id`; `ask` then fails with reason "cancelled". */
  cancel: (id: number) => invoke<void>("ai_cancel", { id }),
};

export type Delimiter = "comma" | "tab" | "semicolon" | "pipe";
export const DELIMITERS: Delimiter[] = ["comma", "tab", "semicolon", "pipe"];

/** Where exported rows come from: a table or view, or one statement run read-only. */
export type DataSource = { kind: "table"; schema: string; name: string } | { kind: "query"; sql: string };

export type DataFormat =
  /** NULL is an empty field and an empty string `""`, as COPY reads them. */
  | { kind: "csv"; delimiter: Delimiter; header: boolean; bom: boolean }
  | { kind: "json" }
  /** INSERT statements into `table`, written as given. */
  | { kind: "insert"; table: string };

export interface Transferred {
  rows: number;
  /** Bytes written to the file, or read from it. */
  bytes: number;
}

export type FileEncoding = "utf8" | "eucKr";

export interface CsvOptions {
  delimiter: Delimiter;
  /** The first row names the columns. */
  header: boolean;
  encoding: FileEncoding;
}

export interface CsvPreview {
  /** The delimiter it was read with: the one asked for, or the likeliest. */
  delimiter: Delimiter;
  /** The first rows as read, the header row included. */
  rows: string[][];
  /** Not valid UTF-8, but reads as EUC-KR. */
  looksEucKr: boolean;
  size: number;
}

export const data = {
  /** Writes rows to `path`, calling `onProgress` with the rows written so far. */
  export: (
    sessionId: number,
    jobId: number,
    source: DataSource,
    format: DataFormat,
    path: string,
    onProgress: (rows: number) => void,
  ) => {
    const progress = new Channel<number>();
    progress.onmessage = onProgress;
    return invoke<Transferred>("data_export", { sessionId, jobId, source, format, path, progress });
  },
  /** The first rows of a CSV file; without a delimiter, the likeliest one is used. */
  preview: (path: string, delimiter: Delimiter | null, encoding: FileEncoding) =>
    invoke<CsvPreview>("data_import_preview", { path, delimiter, encoding }),
  /**
   * Appends a CSV file's rows to a table in one transaction. `columns` names,
   * for each column of the file, the table column it fills, or null to skip it.
   * `onProgress` gets the bytes read so far.
   */
  import: (
    sessionId: number,
    jobId: number,
    schema: string,
    table: string,
    path: string,
    options: CsvOptions,
    columns: (string | null)[],
    onProgress: (bytes: number) => void,
  ) => {
    const progress = new Channel<number>();
    progress.onmessage = onProgress;
    return invoke<Transferred>("data_import", {
      sessionId,
      jobId,
      schema,
      table,
      path,
      options,
      columns,
      progress,
    });
  },
  /** Stops an export or import; it then fails with query_canceled. */
  cancel: (jobId: number) => invoke<void>("data_transfer_cancel", { jobId }),
};

/** A range in editor text, in UTF-16 code units like JavaScript string indices. */
export interface TextRange {
  start: number;
  end: number;
}

export const sqlText = {
  /** The statement to run for a cursor at `cursor`, like DataGrip picks it. */
  statementAt: (sql: string, cursor: number) =>
    invoke<TextRange | null>("sql_statement_at", { sql, cursor }),
  /** Every statement in `sql`, ignoring semicolons in strings, comments and `$$` bodies. */
  split: (sql: string) => invoke<TextRange[]>("sql_split", { sql }),
};

export function isAppError(error: unknown): error is AppError {
  return typeof error === "object" && error !== null && "kind" in error;
}

/** Words an error for the user. Server and git messages pass through as they are. */
export function errorMessage(error: unknown, t: Messages): string {
  if (!isAppError(error)) return error instanceof Error ? error.message : String(error);
  switch (error.kind) {
    case "closed":
      return t.errors.closed;
    case "refused":
      return t.errors.refused[error.reason] ?? error.message;
    case "ai":
      switch (error.reason) {
        case "other":
          return error.message;
        case "network":
          return t.errors.ai.network(error.message);
        case "badApiKey":
          return t.errors.ai.badApiKey(error.message);
        default:
          return t.errors.ai[error.reason];
      }
    default:
      return error.message;
  }
}
