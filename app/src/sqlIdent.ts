import type { Table } from "./api";

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

/** Splits `a, "B c", d` into names, unquoting quoted ones. */
export function splitNames(list: string): string[] {
  const names: string[] = [];
  let current = "";
  let quoted = false;
  for (let i = 0; i < list.length; i++) {
    const ch = list[i];
    if (ch === '"') {
      if (quoted && list[i + 1] === '"') {
        current += '"';
        i++;
      } else {
        quoted = !quoted;
      }
    } else if (ch === "," && !quoted) {
      names.push(current.trim());
      current = "";
    } else {
      current += ch;
    }
  }
  names.push(current.trim());
  return names.filter(Boolean);
}
