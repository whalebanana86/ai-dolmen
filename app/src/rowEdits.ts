import { CellValue, RowChange, RowChanges, RowSource } from "./api";

export type TableSource = Extract<RowSource, { kind: "table" }>;
type Row = (string | null)[];

/**
 * Changes to a result's rows that aren't saved yet. Grid rows are the
 * result's rows, then the added ones.
 */
export interface PendingRows {
  /** New cell values of the result's rows, by row and then column. */
  edits: Record<number, Record<number, string | null>>;
  /** Result rows to delete. */
  deleted: number[];
  /** Rows to add; a cell left undefined gets its column's default. */
  added: (string | null | undefined)[][];
}

export const NO_CHANGES: PendingRows = { edits: {}, deleted: [], added: [] };

/** How many rows the changes touch. */
export function changeCount(pending: PendingRows) {
  const edited = Object.keys(pending.edits).filter((row) => !pending.deleted.includes(Number(row)));
  return edited.length + pending.deleted.length + pending.added.length;
}

/** A grid cell's value with the changes made; undefined is an added row's default. */
export function cellValue(rows: Row[], pending: PendingRows, row: number, column: number) {
  if (row >= rows.length) return pending.added[row - rows.length]?.[column];
  const edited = pending.edits[row];
  return edited && column in edited ? edited[column] : rows[row][column];
}

/** Whether a grid cell holds a change. */
export function isChanged(rows: Row[], pending: PendingRows, row: number, column: number) {
  if (row >= rows.length) return pending.added[row - rows.length]?.[column] !== undefined;
  return column in (pending.edits[row] ?? {});
}

/** Gives a cell a value; setting a result row's cell back to what it was drops the edit. */
export function setCell(
  rows: Row[],
  pending: PendingRows,
  row: number,
  column: number,
  value: string | null | undefined,
): PendingRows {
  if (row >= rows.length) {
    const index = row - rows.length;
    const added = pending.added.map((cells, i) => (i === index ? cells.map((c, j) => (j === column ? value : c)) : cells));
    return { ...pending, added };
  }
  const cells = { ...pending.edits[row] };
  if (value === undefined || value === rows[row][column]) delete cells[column];
  else cells[column] = value;
  const edits = { ...pending.edits };
  if (Object.keys(cells).length === 0) delete edits[row];
  else edits[row] = cells;
  return { ...pending, edits };
}

/** Deletes a result row or keeps it again; an added row is just dropped. */
export function toggleDelete(rows: Row[], pending: PendingRows, row: number): PendingRows {
  if (row >= rows.length) {
    return { ...pending, added: pending.added.filter((_, i) => i !== row - rows.length) };
  }
  const deleted = pending.deleted.includes(row)
    ? pending.deleted.filter((r) => r !== row)
    : [...pending.deleted, row];
  return { ...pending, deleted };
}

export function addRow(pending: PendingRows, width: number): PendingRows {
  return { ...pending, added: [...pending.added, Array<undefined>(width).fill(undefined)] };
}

/** What saving sends, and the grid row each change is about. */
export function toChanges(source: TableSource, rows: Row[], pending: PendingRows) {
  const name = (column: number) => source.columns[column]!.name;
  // Rows are found by the key they were read with, even if it's being changed.
  const key = (row: number): CellValue[] => source.key.map((c) => ({ column: name(c), value: rows[row][c] }));
  const changes: RowChange[] = [];
  const rowOf: number[] = [];
  // Deletes go first, so an added row can take a deleted one's key.
  for (const row of [...pending.deleted].sort((a, b) => a - b)) {
    changes.push({ kind: "delete", key: key(row) });
    rowOf.push(row);
  }
  for (const [r, cells] of Object.entries(pending.edits)) {
    const row = Number(r);
    if (pending.deleted.includes(row)) continue;
    changes.push({
      kind: "update",
      key: key(row),
      cells: Object.entries(cells).map(([c, value]) => ({ column: name(Number(c)), old: rows[row][Number(c)], new: value })),
    });
    rowOf.push(row);
  }
  pending.added.forEach((cells, i) => {
    changes.push({
      kind: "insert",
      cells: cells.flatMap((value, c) => (value === undefined || !source.columns[c] ? [] : [{ column: name(c), value }])),
    });
    rowOf.push(rows.length + i);
  });
  const returning = source.columns.flatMap((column) => (column ? [column.name] : []));
  const request: RowChanges = { schema: source.schema, table: source.table, changes, returning };
  return { request, rowOf };
}

/**
 * The result's rows once `saved` came back for `changes`: deleted ones gone,
 * changed ones as the table has them now, added ones at the end. Values the
 * query computes stay as they were read; an added row has none.
 */
export function savedRows(
  source: TableSource,
  rows: Row[],
  changes: RowChange[],
  rowOf: number[],
  saved: (Row | null)[],
): Row[] {
  const shown = source.columns.flatMap((column, c) => (column ? [c] : []));
  const now = rows.map((row) => [...row]);
  const added: Row[] = [];
  const deleted = new Set<number>();
  changes.forEach((change, i) => {
    const values = saved[i];
    if (change.kind === "delete") deleted.add(rowOf[i]);
    if (change.kind === "delete" || !values) return;
    const row = change.kind === "insert" ? Array<string | null>(source.columns.length).fill(null) : now[rowOf[i]];
    shown.forEach((c, j) => {
      row[c] = values[j];
    });
    if (change.kind === "insert") added.push(row);
  });
  return [...now.filter((_, r) => !deleted.has(r)), ...added];
}
