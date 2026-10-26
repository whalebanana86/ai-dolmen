// Typed wrappers around the Tauri commands in src-tauri/src/lib.rs.
// The shapes mirror dolmen-core and are pinned by crates/dolmen-core/tests/wire_format.rs.
import { invoke } from "@tauri-apps/api/core";

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
}

export type StatementResult =
  | {
      kind: "rows";
      columns: string[];
      /** Values in PostgreSQL's text format; null is SQL NULL. */
      rows: (string | null)[][];
      /** Rows the statement returned, which can exceed rows.length. */
      totalRows: number;
    }
  | { kind: "command"; rowsAffected: number };

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
}

export interface View {
  name: string;
  materialized: boolean;
  columns: Column[];
  definition: string;
  comment: string | null;
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
}

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
  | { kind: "storage"; message: string };

export function appVersion() {
  return invoke<string>("app_version");
}

export const pg = {
  /** Without a password in `config`, the stored password of `savedId` is used. */
  connect: (config: ConnectConfig, savedId: string | null = null) =>
    invoke<SessionInfo>("pg_connect", { config, savedId }),
  execute: (sessionId: number, sql: string) =>
    invoke<StatementResult[]>("pg_execute", { sessionId, sql }),
  introspect: (sessionId: number) => invoke<Introspection>("pg_introspect", { sessionId }),
  cancel: (sessionId: number) => invoke<void>("pg_cancel", { sessionId }),
  disconnect: (sessionId: number) => invoke<void>("pg_disconnect", { sessionId }),
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

export interface SnapshotStatus {
  /** The Git repository holding the folder, if any. */
  repository: { root: string; branch: string | null } | null;
  /** What writing the snapshot now would add, change or remove. */
  changes: FileChange[];
  /** Whether the folder already holds a snapshot. */
  initialized: boolean;
}

export const snapshots = {
  status: (sessionId: number, dir: string) =>
    invoke<SnapshotStatus>("snapshot_status", { sessionId, dir }),
  /** Writes the snapshot into `dir`, and commits it when a message is given. */
  save: (sessionId: number, dir: string, commitMessage: string | null) =>
    invoke<{ commit: string | null }>("snapshot_save", { sessionId, dir, commitMessage }),
  /** The CREATE statement for one object, as written in snapshots. */
  createStatement: (schema: string, object: SchemaObject) =>
    invoke<string>("ddl_create_statement", { schema, object }),
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

export function errorMessage(error: unknown): string {
  if (!isAppError(error)) return String(error);
  return error.kind === "closed" ? "The connection is closed." : error.message;
}
