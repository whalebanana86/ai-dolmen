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
