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

export interface TableOutline {
  schema: string;
  name: string;
  columns: string[];
}

export interface SchemaOutline {
  /** The schema unqualified names resolve to first, usually "public". */
  defaultSchema: string;
  tables: TableOutline[];
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
  schemaOutline: (sessionId: number) => invoke<SchemaOutline>("pg_schema_outline", { sessionId }),
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
}

export type PasswordChange = { kind: "keep" } | { kind: "set"; password: string } | { kind: "forget" };

export const connections = {
  list: () => invoke<SavedConnection[]>("connections_list"),
  save: (connection: SavedConnection, password: PasswordChange) =>
    invoke<SavedConnection>("connections_save", { connection, password }),
  delete: (id: string) => invoke<void>("connections_delete", { id }),
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
