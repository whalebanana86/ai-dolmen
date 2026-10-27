import type { Table } from "./api";

// PostgreSQL's reserved key words: names that must be quoted to be used as identifiers.
const RESERVED = new Set(
  (
    "all analyse analyze and any array as asc asymmetric both case cast check collate column " +
    "constraint create current_catalog current_date current_role current_time current_timestamp " +
    "current_user default deferrable desc distinct do else end except false fetch for foreign from " +
    "grant group having in initially intersect into lateral leading limit localtime localtimestamp " +
    "not null offset on only or order placing primary references returning select session_user " +
    "some symmetric system_user table then to trailing true union unique user using variadic when " +
    "where window with"
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
