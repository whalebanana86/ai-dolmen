import type { Engine } from "./api";

/**
 * AI is left out of releases for now. A development build shows it when
 * started with VITE_DOLMEN_AI=1.
 */
export const AI_ENABLED = import.meta.env.VITE_DOLMEN_AI === "1";

/** What the workspace offers for a connection's engine. */
export interface Capabilities {
  /** Statements are SQL: completion, quoting and the SQL highlighter. */
  sql: boolean;
  /** The structure is schemas of tables (MongoDB's databases of collections) rather than keys. */
  schemaTree: boolean;
  /** Plans, risk checks before running, snapshots, the detail editor, grid edits, import and export. */
  postgresOnly: boolean;
  /** Foreign keys to draw. */
  erd: boolean;
  /** CREATE statements to show. */
  ddl: boolean;
}

export function capabilities(engine: Engine): Capabilities {
  const postgres = engine === "postgres";
  const mysql = engine === "mysql" || engine === "mariadb";
  const mongo = engine === "mongodb" || engine === "documentdb";
  return {
    sql: postgres || mysql,
    schemaTree: postgres || mysql || mongo,
    postgresOnly: postgres,
    erd: postgres || mysql,
    ddl: postgres || mysql,
  };
}
