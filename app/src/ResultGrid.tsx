import { KeyboardEvent, PointerEvent, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

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

interface Cell {
  row: number;
  column: number;
}

interface Props {
  columns: string[];
  /** Values in PostgreSQL's text format; null is SQL NULL. */
  rows: (string | null)[][];
}

/**
 * A result grid that only renders the rows in view, so large results stay fast.
 * Click a cell to select it, move with the arrow keys, copy with ⌘/Ctrl+C,
 * and drag a header edge to resize a column.
 */
export function ResultGrid({ columns, rows }: Props) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [widths, setWidths] = useState(() => initialWidths(columns, rows));
  const [selected, setSelected] = useState<Cell | null>(null);

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    paddingStart: HEADER_HEIGHT,
    scrollPaddingStart: HEADER_HEIGHT,
    overscan: 20,
  });
  const totalWidth = ROW_NUMBER_WIDTH + widths.reduce((sum, width) => sum + width, 0);

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (!selected) return;
    const move = ARROW_MOVES[event.key];
    if (move) {
      event.preventDefault();
      const next = {
        row: clamp(selected.row + move.rows, 0, rows.length - 1),
        column: clamp(selected.column + move.columns, 0, columns.length - 1),
      };
      setSelected(next);
      virtualizer.scrollToIndex(next.row);
      scrollColumnIntoView(next.column);
    } else if ((event.metaKey || event.ctrlKey) && event.key === "c") {
      event.preventDefault();
      navigator.clipboard
        .writeText(rows[selected.row][selected.column] ?? "NULL")
        .catch((error) => console.error("copy failed", error));
    }
  }

  function scrollColumnIntoView(column: number) {
    const scroller = scrollRef.current;
    if (!scroller) return;
    // The sticky row number column covers the left edge of the scroll area.
    const left = widths.slice(0, column).reduce((sum, width) => sum + width, 0);
    const right = ROW_NUMBER_WIDTH + left + widths[column];
    if (left < scroller.scrollLeft) {
      scroller.scrollLeft = left;
    } else if (right > scroller.scrollLeft + scroller.clientWidth) {
      scroller.scrollLeft = right - scroller.clientWidth;
    }
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

  return (
    <div ref={scrollRef} className="grid" tabIndex={0} onKeyDown={onKeyDown}>
      <div className="grid-body" style={{ width: totalWidth, height: virtualizer.getTotalSize() }}>
        <div className="grid-header" style={{ width: totalWidth, height: HEADER_HEIGHT }}>
          <div className="grid-cell row-number" style={{ width: ROW_NUMBER_WIDTH }}>
            #
          </div>
          {columns.map((name, c) => (
            <div key={c} className="grid-cell" style={{ width: widths[c] }} title={name}>
              {name}
              <div className="resize-handle" onPointerDown={(e) => startResize(c, e)} />
            </div>
          ))}
        </div>

        {virtualizer.getVirtualItems().map((item) => (
          <div
            key={item.key}
            className="grid-row"
            style={{ top: item.start, height: ROW_HEIGHT, width: totalWidth }}
          >
            <div className="grid-cell row-number" style={{ width: ROW_NUMBER_WIDTH }}>
              {item.index + 1}
            </div>
            {rows[item.index].map((value, c) => {
              const isSelected = selected?.row === item.index && selected.column === c;
              return (
                <div
                  key={c}
                  className={`grid-cell${value === null ? " null" : ""}${isSelected ? " selected" : ""}`}
                  style={{ width: widths[c] }}
                  title={value ?? undefined}
                  onMouseDown={() => setSelected({ row: item.index, column: c })}
                >
                  {value ?? "NULL"}
                </div>
              );
            })}
          </div>
        ))}
      </div>
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
