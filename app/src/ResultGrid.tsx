import { KeyboardEvent, PointerEvent, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { ContextMenu, MenuItem } from "./ContextMenu";
import { PendingRows, addRow, cellValue, isChanged, setCell, toggleDelete } from "./rowEdits";
import { useT } from "./settings";

const ROW_HEIGHT = 24;
const HEADER_HEIGHT = 28;
const ROW_NUMBER_WIDTH = 56;
/** Width of one character of the 12px monospace grid font. */
const CHAR_WIDTH = 7.2;
const CELL_PADDING = 18;
const MIN_COLUMN_WIDTH = 60;
const MAX_INITIAL_COLUMN_WIDTH = 400;
/** Rows looked at to size columns. */
const SAMPLE_ROWS = 200;
/** The cell editor is at least this wide, so short columns still take a value comfortably. */
const MIN_EDITOR_WIDTH = 160;
const EDITOR_LINE_HEIGHT = 17;
/** Lines the cell editor shows before it scrolls. */
const EDITOR_MAX_LINES = 8;

interface Cell {
  row: number;
  column: number;
}

/** What the grid needs to change rows. */
export interface GridEditing {
  pending: PendingRows;
  /** Whether each column's cells take values. */
  editable: boolean[];
  /** Columns the query computes, which an added row has no value for until it's run again. */
  computed: boolean[];
  /** Changes are refused for now, as on a read-only connection. */
  locked: boolean;
  onChange: (pending: PendingRows) => void;
  /** Saves the changes, for ⌘S. */
  onSave: () => void;
}

interface Props {
  columns: string[];
  /** Values in PostgreSQL's text format; null is SQL NULL. */
  rows: (string | null)[][];
  /** Shown when hovering a column's header, after its name. */
  columnNotes?: (string | null)[];
  /** Given when the rows can be changed. */
  editing?: GridEditing;
  /** Someone tried to change what can't be: a column, or the whole result (null). */
  onCantEdit?: (column: number | null) => void;
  /** A row to scroll to and mark, such as one that failed to save. */
  problemRow?: number | null;
}

/** The cell being edited: the text typed so far and what it started as. */
interface Editor extends Cell {
  text: string;
  initial: string;
}

/**
 * A result grid that only renders the rows in view, so large results stay fast.
 * Click a cell to select it, move with the arrow keys, copy with ⌘/Ctrl+C,
 * and drag a header edge to resize a column. When the rows can be changed,
 * double-click a cell or press Enter to edit it, and ⌘⌫ deletes a row.
 */
export function ResultGrid({ columns, rows, columnNotes, editing, onCantEdit, problemRow }: Props) {
  const t = useT();
  const scrollRef = useRef<HTMLDivElement>(null);
  const [widths, setWidths] = useState(() => initialWidths(columns, rows));
  const [selected, setSelected] = useState<Cell | null>(null);
  const [editor, setEditor] = useState<Editor | null>(null);
  // Read on blur, which can come after Escape has closed the editor.
  const editorRef = useRef<Editor | null>(null);
  editorRef.current = editor;
  const [menu, setMenu] = useState<{ x: number; y: number; cell: Cell } | null>(null);
  const pending = editing?.pending;
  const count = rows.length + (pending?.added.length ?? 0);

  const virtualizer = useVirtualizer({
    count,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    paddingStart: HEADER_HEIGHT,
    scrollPaddingStart: HEADER_HEIGHT,
    overscan: 20,
  });
  const totalWidth = ROW_NUMBER_WIDTH + widths.reduce((sum, width) => sum + width, 0);

  // A new row starts being edited at its first cell that takes a value,
  // before the next paint, so keys pressed right away reach the editor.
  const addedCount = pending?.added.length ?? 0;
  const lastAdded = useRef(addedCount);
  useLayoutEffect(() => {
    if (addedCount > lastAdded.current && editing) {
      const row = rows.length + addedCount - 1;
      const column = Math.max(0, editing.editable.indexOf(true));
      setSelected({ row, column });
      virtualizer.scrollToIndex(row);
      scrollColumnIntoView(column);
      startEdit({ row, column });
    }
    lastAdded.current = addedCount;
  }, [addedCount]);

  // Saving can remove rows, the selected one among them.
  useEffect(() => {
    if (selected && selected.row >= count) setSelected(null);
  }, [count]);

  useEffect(() => {
    if (problemRow == null || problemRow >= count) return;
    setSelected({ row: problemRow, column: selected?.column ?? 0 });
    virtualizer.scrollToIndex(problemRow, { align: "center" });
  }, [problemRow]);

  const isDeleted = (row: number) => !!pending?.deleted.includes(row);
  const valueAt = (row: number, column: number) =>
    pending ? cellValue(rows, pending, row, column) : rows[row][column];

  function change(next: (pending: PendingRows) => PendingRows) {
    if (!editing) return;
    if (editing.locked) {
      onCantEdit?.(null);
      return;
    }
    editing.onChange(next(editing.pending));
  }

  function startEdit(cell: Cell) {
    if (!editing || editing.locked) {
      onCantEdit?.(null);
      return;
    }
    if (!editing.editable[cell.column]) {
      onCantEdit?.(cell.column);
      return;
    }
    if (isDeleted(cell.row)) return;
    const text = valueAt(cell.row, cell.column) ?? "";
    setEditor({ ...cell, text, initial: text });
  }

  /** Keeps what was typed; an untouched editor leaves NULL or the default alone. */
  function commitEdit() {
    const current = editorRef.current;
    editorRef.current = null;
    setEditor(null);
    if (!current || current.text === current.initial) return;
    change((pending) => setCell(rows, pending, current.row, current.column, current.text));
  }

  function cancelEdit() {
    editorRef.current = null;
    setEditor(null);
    scrollRef.current?.focus();
  }

  /** The next column along the row that takes a value, or null. */
  function nextEditable(column: number, step: number) {
    for (let c = column + step; c >= 0 && c < columns.length; c += step) {
      if (editing?.editable[c]) return c;
    }
    return null;
  }

  function onEditorKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    event.stopPropagation();
    // Enter also ends composing Hangul; that one isn't for the editor.
    if (event.nativeEvent.isComposing || event.keyCode === 229) return;
    const current = editorRef.current;
    if (!current) return;
    if (event.key === "Escape") {
      event.preventDefault();
      cancelEdit();
    } else if (event.key === "Enter" && !event.shiftKey && !event.altKey) {
      event.preventDefault();
      commitEdit();
      scrollRef.current?.focus();
    } else if (event.key === "Tab") {
      event.preventDefault();
      commitEdit();
      const column = nextEditable(current.column, event.shiftKey ? -1 : 1);
      if (column === null) {
        scrollRef.current?.focus();
        return;
      }
      const cell = { row: current.row, column };
      setSelected(cell);
      scrollColumnIntoView(column);
      startEdit(cell);
    }
  }

  function copy(cell: Cell) {
    const value = valueAt(cell.row, cell.column);
    navigator.clipboard
      .writeText(value === undefined ? "DEFAULT" : (value ?? "NULL"))
      .catch((error) => console.error("copy failed", error));
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (!selected) return;
    const mod = event.metaKey || event.ctrlKey;
    const move = ARROW_MOVES[event.key];
    if (move) {
      event.preventDefault();
      const next = {
        row: clamp(selected.row + move.rows, 0, count - 1),
        column: clamp(selected.column + move.columns, 0, columns.length - 1),
      };
      setSelected(next);
      virtualizer.scrollToIndex(next.row);
      scrollColumnIntoView(next.column);
    } else if (mod && event.key === "c") {
      event.preventDefault();
      copy(selected);
    } else if (mod && event.key === "s" && editing) {
      event.preventDefault();
      editing.onSave();
    } else if (mod && event.key === "Backspace" && editing) {
      event.preventDefault();
      change((pending) => toggleDelete(rows, pending, selected.row));
    } else if ((event.key === "Enter" || event.key === "F2") && !mod) {
      event.preventDefault();
      startEdit(selected);
    }
  }

  function scrollColumnIntoView(column: number) {
    const scroller = scrollRef.current;
    if (!scroller) return;
    // The sticky row number column covers the left edge of the scroll area.
    const left = columnLeft(column) - ROW_NUMBER_WIDTH;
    const right = ROW_NUMBER_WIDTH + left + widths[column];
    if (left < scroller.scrollLeft) {
      scroller.scrollLeft = left;
    } else if (right > scroller.scrollLeft + scroller.clientWidth) {
      scroller.scrollLeft = right - scroller.clientWidth;
    }
  }

  function columnLeft(column: number) {
    return ROW_NUMBER_WIDTH + widths.slice(0, column).reduce((sum, width) => sum + width, 0);
  }

  function startResize(column: number, event: PointerEvent<HTMLDivElement>) {
    event.preventDefault();
    const handle = event.currentTarget;
    const startX = event.clientX;
    const startWidth = widths[column];
    handle.setPointerCapture(event.pointerId);

    const move = (e: globalThis.PointerEvent) => {
      const width = Math.max(MIN_COLUMN_WIDTH, startWidth + e.clientX - startX);
      setWidths((current) => current.map((w, i) => (i === column ? width : w)));
    };
    const stop = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", stop);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", stop);
  }

  function menuItems(cell: Cell): MenuItem[] {
    const items: MenuItem[] = [{ label: t.grid.copy, onSelect: () => copy(cell) }];
    if (!editing) return items;
    const added = cell.row >= rows.length;
    const deleted = isDeleted(cell.row);
    if (!deleted) {
      items.push({ label: t.grid.edit, onSelect: () => startEdit(cell) });
      if (editing.editable[cell.column] && !editing.locked) {
        items.push({ label: t.grid.setNull, onSelect: () => change((p) => setCell(rows, p, cell.row, cell.column, null)) });
      }
      if (pending && isChanged(rows, pending, cell.row, cell.column)) {
        const original = added ? undefined : rows[cell.row][cell.column];
        items.push({
          label: added ? t.grid.setDefault : t.grid.revert,
          onSelect: () => change((p) => setCell(rows, p, cell.row, cell.column, original)),
        });
      }
    }
    items.push(
      { separator: true },
      {
        label: added ? t.grid.removeAdded : deleted ? t.grid.undelete : t.grid.deleteRow,
        danger: !added && !deleted,
        onSelect: () => change((p) => toggleDelete(rows, p, cell.row)),
      },
      { label: t.grid.addRow, onSelect: () => change((p) => addRow(p, columns.length)) },
    );
    return items;
  }

  const editorLines = editor ? Math.min(editor.text.split("\n").length, EDITOR_MAX_LINES) : 1;

  return (
    <div ref={scrollRef} className="grid" tabIndex={0} onKeyDown={onKeyDown}>
      <div className="grid-body" style={{ width: totalWidth, height: virtualizer.getTotalSize() }}>
        <div className="grid-header" style={{ width: totalWidth, height: HEADER_HEIGHT }}>
          <div className="grid-cell row-number" style={{ width: ROW_NUMBER_WIDTH }}>
            #
          </div>
          {columns.map((name, c) => (
            <div
              key={c}
              className="grid-cell"
              style={{ width: widths[c] }}
              title={columnNotes?.[c] ? `${name}\n${columnNotes[c]}` : name}
            >
              {name}
              <div className="resize-handle" onPointerDown={(e) => startResize(c, e)} />
            </div>
          ))}
        </div>

        {virtualizer.getVirtualItems().map((item) => {
          const row = item.index;
          const rowClass = [
            "grid-row",
            row >= rows.length && "added",
            isDeleted(row) && "deleted",
            row === problemRow && "problem",
          ]
            .filter(Boolean)
            .join(" ");
          return (
            <div key={item.key} className={rowClass} style={{ top: item.start, height: ROW_HEIGHT, width: totalWidth }}>
              <div className="grid-cell row-number" style={{ width: ROW_NUMBER_WIDTH }}>
                {row + 1}
              </div>
              {columns.map((_, c) => {
                const value = valueAt(row, c);
                const unknown = row >= rows.length && !!editing?.computed[c];
                const isSelected = selected?.row === row && selected.column === c;
                const cellClass = [
                  "grid-cell",
                  value === null && "null",
                  value === undefined && "default",
                  pending && row < rows.length && isChanged(rows, pending, row, c) && "edited",
                  isSelected && "selected",
                ]
                  .filter(Boolean)
                  .join(" ");
                return (
                  <div
                    key={c}
                    className={cellClass}
                    style={{ width: widths[c] }}
                    title={value ?? undefined}
                    onMouseDown={() => setSelected({ row, column: c })}
                    onDoubleClick={() => startEdit({ row, column: c })}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      setSelected({ row, column: c });
                      setMenu({ x: e.clientX, y: e.clientY, cell: { row, column: c } });
                    }}
                  >
                    {unknown ? "" : value === undefined ? "DEFAULT" : (value ?? "NULL")}
                  </div>
                );
              })}
            </div>
          );
        })}

        {editor && (
          <textarea
            className="grid-editor"
            autoFocus
            spellCheck={false}
            value={editor.text}
            aria-label={columns[editor.column]}
            onChange={(e) => setEditor({ ...editor, text: e.target.value })}
            onKeyDown={onEditorKeyDown}
            onBlur={commitEdit}
            onFocus={(e) => e.currentTarget.select()}
            style={{
              top: HEADER_HEIGHT + editor.row * ROW_HEIGHT,
              left: columnLeft(editor.column),
              width: Math.max(widths[editor.column], MIN_EDITOR_WIDTH),
              height: Math.max(ROW_HEIGHT, editorLines * EDITOR_LINE_HEIGHT + 6),
            }}
          />
        )}
      </div>
      {menu && <ContextMenu x={menu.x} y={menu.y} items={menuItems(menu.cell)} onClose={() => setMenu(null)} />}
    </div>
  );
}

const ARROW_MOVES: Record<string, { rows: number; columns: number }> = {
  ArrowUp: { rows: -1, columns: 0 },
  ArrowDown: { rows: 1, columns: 0 },
  ArrowLeft: { rows: 0, columns: -1 },
  ArrowRight: { rows: 0, columns: 1 },
};

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}

/** Sizes each column to fit its header and the values in the first rows. */
function initialWidths(columns: string[], rows: (string | null)[][]) {
  const sample = rows.slice(0, SAMPLE_ROWS);
  return columns.map((name, c) => {
    const chars = Math.max(displayWidth(name), ...sample.map((row) => displayWidth(row[c] ?? "NULL")));
    return clamp(Math.ceil(chars * CHAR_WIDTH) + CELL_PADDING, MIN_COLUMN_WIDTH, MAX_INITIAL_COLUMN_WIDTH);
  });
}

/** Width in monospace cells; Hangul, CJK and other wide characters take two. */
function displayWidth(text: string) {
  let width = 0;
  // Anything longer is clamped to the maximum column width anyway.
  for (const char of text.slice(0, 80)) {
    width += isWide(char.codePointAt(0)!) ? 2 : 1;
  }
  return width;
}

function isWide(code: number) {
  return (
    (code >= 0x1100 && code <= 0x115f) || // Hangul Jamo
    (code >= 0x2e80 && code <= 0xa4cf) || // CJK, Kana, Yi
    (code >= 0xac00 && code <= 0xd7a3) || // Hangul syllables
    (code >= 0xf900 && code <= 0xfaff) || // CJK compatibility ideographs
    (code >= 0xff00 && code <= 0xff60) || // Fullwidth forms
    (code >= 0x1f300 && code <= 0x1faff) || // Emoji
    (code >= 0x20000 && code <= 0x3fffd) // CJK extensions
  );
}
