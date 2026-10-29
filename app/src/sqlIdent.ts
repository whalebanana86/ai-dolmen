import type { Engine, Table } from "./api";

// PostgreSQL's key words that aren't unreserved: names that must be quoted to be used as identifiers.
const RESERVED = new Set(
  (
    "all analyse analyze and any array as asc asymmetric both case cast check collate column " +
    "constraint create current_catalog current_date current_role current_time current_timestamp " +
    "current_user default deferrable desc distinct do else end except false fetch for foreign from " +
    "grant group having in initially intersect into lateral leading limit localtime localtimestamp " +
    "not null offset on only or order placing primary references returning select session_user " +
    "some symmetric system_user table then to trailing true union unique user using variadic when " +
    "where window with " +
    // Type and function name keywords, and column name keywords: PostgreSQL
    // quotes every keyword that isn't unreserved, and so do we.
    "authorization binary collation concurrently cross current_schema freeze full ilike inner is isnull join left like natural notnull outer overlaps right similar tablesample verbose " +
    "between bigint bit boolean char character coalesce dec decimal exists extract float greatest grouping inout int integer interval json json_array json_arrayagg json_exists json_object json_objectagg json_query json_scalar json_serialize json_table json_value least merge_action national nchar none normalize nullif numeric out overlay position precision real row setof smallint substring time timestamp treat trim values varchar xmlattributes xmlconcat xmlelement xmlexists xmlforest xmlnamespaces xmlparse xmlpi xmlroot xmlserialize xmltable"
  ).split(" "),
);

/** Quotes a name for the engine: in backquotes for MySQL and MariaDB, else as PostgreSQL does. */
export function quoteIdentFor(engine: Engine, name: string) {
  return engine === "mysql" || engine === "mariadb" ? `\`${name.replace(/`/g, "``")}\`` : quoteIdent(name);
}

/** Quotes a PostgreSQL identifier unless it is a plain lower-case name. */
export function quoteIdent(name: string) {
  return /^[a-z_][a-z0-9_$]*$/.test(name) && !RESERVED.has(name)
    ? name
    : `"${name.replace(/"/g, '""')}"`;
}

/** The columns of the table's primary key, read from its definition. */
export function primaryKeyColumns(table: Table): string[] {
  const definition = table.constraints.find((c) => c.kind === "primaryKey")?.definition ?? "";
  const list = /^PRIMARY KEY \((.*)\)/.exec(definition)?.[1] ?? "";
  return splitNames(list);
}

/** Splits `a, "B c", d` into names, unquoting quoted ones (MySQL's `backquoted` too). */
export function splitNames(list: string): string[] {
  const names: string[] = [];
  let current = "";
  let quote: string | null = null;
  for (let i = 0; i < list.length; i++) {
    const ch = list[i];
    if (ch === quote || (quote === null && (ch === '"' || ch === "`"))) {
      if (quote && list[i + 1] === quote) {
        current += quote;
        i++;
      } else {
        quote = quote ? null : ch;
      }
    } else if (ch === "," && quote === null) {
      names.push(current.trim());
      current = "";
    } else {
      current += ch;
    }
  }
  names.push(current.trim());
  return names.filter(Boolean);
}

const IDENT = String.raw`(?:"(?:[^"]|"")+"|[\p{L}_][\p{L}\p{N}_$]*)`;
const NAME = String.raw`${IDENT}(?:\s*\.\s*${IDENT})?`;
const TARGETS = [
  String.raw`^(?:INSERT|MERGE)\s+INTO\s+(${NAME})`,
  String.raw`^UPDATE\s+(?:ONLY\s+)?(${NAME})`,
  String.raw`^DELETE\s+FROM\s+(?:ONLY\s+)?(${NAME})`,
  String.raw`^TRUNCATE\s+(?:TABLE\s+)?(?:ONLY\s+)?(${NAME})`,
  String.raw`^COPY\s+(${NAME})`,
  // An index is about the table it is on.
  String.raw`^CREATE\s+(?:UNIQUE\s+)?INDEX\b[\s\S]*?\bON\s+(?:ONLY\s+)?(${NAME})`,
  String.raw`^(?:CREATE|ALTER|DROP)\s+(?:OR\s+REPLACE\s+)?(?:(?:UNLOGGED|TEMP|TEMPORARY|UNIQUE)\s+)*` +
    String.raw`(?:TABLE|VIEW|MATERIALIZED\s+VIEW|INDEX(?:\s+CONCURRENTLY)?|SEQUENCE|FUNCTION|PROCEDURE|TYPE|SCHEMA)\s+` +
    String.raw`(?:IF\s+(?:NOT\s+)?EXISTS\s+)?(${NAME})`,
].map((pattern) => new RegExp(pattern, "iu"));
const FROM = new RegExp(String.raw`\bFROM\s+(?:ONLY\s+)?(${NAME})(\s*\()?`, "giu");

/**
 * The table a statement mainly works on, as written in it: the target of an
 * INSERT, UPDATE, DELETE or DDL statement, else the first table after FROM.
 * Null when there is none, as in `SELECT version()`.
 */
export function statementTable(sql: string): string | null {
  // Leading comments aside, and string literals blanked so their words don't count.
  const text = sql
    .replace(/^(?:\s|--[^\n]*\n|\/\*[\s\S]*?\*\/)*/, "")
    .replace(/'(?:[^']|'')*'/g, "''");
  for (const pattern of TARGETS) {
    const match = pattern.exec(text);
    if (match) return match[1];
  }
  for (const match of text.matchAll(FROM)) {
    // FROM generate_series(...) names a function, not a table.
    if (!match[2]) return match[1];
  }
  return null;
}
