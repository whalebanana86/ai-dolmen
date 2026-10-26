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

export type DbError =
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
  | { kind: "closed" };

export function appVersion() {
  return invoke<string>("app_version");
}

export const pg = {
  connect: (config: ConnectConfig) => invoke<SessionInfo>("pg_connect", { config }),
  execute: (sessionId: number, sql: string) =>
    invoke<StatementResult[]>("pg_execute", { sessionId, sql }),
  cancel: (sessionId: number) => invoke<void>("pg_cancel", { sessionId }),
  disconnect: (sessionId: number) => invoke<void>("pg_disconnect", { sessionId }),
};

export function isDbError(error: unknown): error is DbError {
  return typeof error === "object" && error !== null && "kind" in error;
}

export function errorMessage(error: unknown): string {
  if (!isDbError(error)) return String(error);
  return error.kind === "closed" ? "The connection is closed." : error.message;
}
